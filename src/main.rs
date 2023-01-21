use clap::{Parser, Subcommand};
use std::{collections::HashMap, sync::Mutex};

use tabled::{object::Rows, style::HorizontalLine, Table};

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Display route info
    Route {
        /// Route number, e.g. `35A`
        #[arg(short, long)]
        route: String,
    },

    /// Display eta info
    Eta {
        /// Route number, e.g. `35A`
        #[arg(short, long)]
        route: String,

        /// Route direction, either `inbound` or `outbound`
        #[arg(short, long)]
        direction: String,

        /// Route service type, always `1`
        #[arg(short, long, default_value = "1")]
        service_type: String,
    },
}

const BASE_URL: &str = "https://data.etabus.gov.hk";
static STOP_ID_NAMES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

async fn load_names() -> Result<(), Box<dyn std::error::Error>> {
    let api_url = "v1/transport/kmb/stop";

    let req_url = format!("{}/{}", BASE_URL, api_url);

    let body = reqwest::get(req_url)
        .await?
        .json::<serde_json::Value>()
        .await?;

    let mut mutext_stop_id_names = STOP_ID_NAMES.lock().unwrap();

    body["data"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .for_each(|data| {
            let stop_id = data["stop"].as_str().unwrap();
            let name_tc = data["name_tc"].as_str().unwrap();

            mutext_stop_id_names.push((String::from(stop_id), String::from(name_tc)));
        });

    mutext_stop_id_names.sort();
    drop(mutext_stop_id_names);
    Ok(())
}

async fn search_route_eta(
    route: &str,
    direction: &str,
    service_type: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // make sure route exists
    search_route_info(route, false).await?;

    let api_url = "v1/transport/kmb/route-stop";
    let req_url = format!(
        "{}/{}/{}/{}/{}",
        BASE_URL, api_url, route, direction, service_type
    );

    let body = reqwest::get(req_url)
        .await?
        .json::<serde_json::Value>()
        .await?;

    let route_ids =
        body["data"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .fold(vec![], |mut cur, data| {
                let seq = data["seq"].as_str().unwrap().parse::<i64>().unwrap();
                let stop_id = data["stop"].as_str().unwrap();

                cur.push((seq, String::from(stop_id)));
                cur
            });

    let api_url = "v1/transport/kmb/route-eta";
    let req_url = format!("{}/{}/{}/{}", BASE_URL, api_url, route, service_type);

    let body = reqwest::get(req_url)
        .await?
        .json::<serde_json::Value>()
        .await?;

    let mut stop_eta = HashMap::new();
    let parse_eta_str = |eta_val: &serde_json::Value| -> String {
        if eta_val.is_string() {
            let eta_str = eta_val.as_str().unwrap();

            let start_idx = eta_str.find('T');
            let end_idx = eta_str.find('+');

            if let (Some(i), Some(j)) = (start_idx, end_idx) {
                return eta_str.get(i + 1..j).unwrap().to_string();
            }
        }
        "".to_string()
    };

    body["data"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .for_each(|data| {
            let dir = data["dir"].as_str().unwrap();
            let dir_match = match direction {
                "outbound" => dir == "O",
                "inbound" => dir == "I",
                _ => false,
            };

            if !dir_match {
                return;
            }

            let seq = data["seq"].as_i64().unwrap();
            let eta_seq = data["eta_seq"].as_i64().unwrap();
            let eta = parse_eta_str(&data["eta"]);

            stop_eta.insert((seq, eta_seq), eta);
        });

    let mutex_stop_id_names = STOP_ID_NAMES.lock().unwrap();
    let mut output = vec![
        ("seq".to_string(), "stop_name", "t1", "t2", "t3")
    ];

    let empty_eta = &"".to_string();
    for (ref_seq, stop_id) in &route_ids {
        let seq = *ref_seq;
        let idx = match mutex_stop_id_names.binary_search(&(stop_id.to_string(), String::new())) {
            Ok(i) => i,
            Err(i) => i,
        };

        let stop_name = &mutex_stop_id_names.get(idx).unwrap().1;

        let first_eta = stop_eta.get(&(seq, 1)).unwrap_or(empty_eta);
        let second_eta = stop_eta.get(&(seq, 2)).unwrap_or(empty_eta);
        let third_eta = stop_eta.get(&(seq, 3)).unwrap_or(empty_eta);

        output.push((
            seq.to_string(),
            stop_name,
            first_eta,
            second_eta,
            third_eta,
        ));
    }

    let mut table = Table::new(output);
    table
        .with(
            tabled::Style::modern()
                .off_horizontal()
                .horizontals([HorizontalLine::new(
                    1,
                    tabled::Style::modern().get_horizontal(),
                )]),
        )
        .with(tabled::Disable::row(Rows::first()));
    println!("{}", table);

    Ok(())
}

async fn get_route(
    route: &str,
    direction: &str,
    service_type: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let api_url = "v1/transport/kmb/route";

    let req_url = format!(
        "{}/{}/{}/{}/{}",
        BASE_URL, api_url, route, direction, service_type
    );

    let body = reqwest::get(req_url)
        .await?
        .json::<serde_json::Value>()
        .await?;

    if body["data"].as_object().unwrap().is_empty() {
        Err("route does not exist")?;
    }

    let orig_tc = &body["data"]["orig_tc"].as_str().unwrap();
    let dest_tc = &body["data"]["dest_tc"].as_str().unwrap();

    Ok((orig_tc.to_string(), dest_tc.to_string()))
}

async fn search_route_info(route: &str, to_print: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (o_orig, o_dest) = get_route(route, "outbound", "1").await?;
    let (i_orig, i_dest) = get_route(route, "inbound", "1").await?;

    let output = vec![
        ("route", "direction", "service_type", "orig", "dest"),
        (route, "outbound", "1", &o_orig, &o_dest),
        (route, "inbound", "1", &i_orig, &i_dest),
    ];

    if !to_print { return Ok(()); }

    let mut table = Table::new(output);
    table
        .with(
            tabled::Style::modern()
                .off_horizontal()
                .horizontals([HorizontalLine::new(
                    1,
                    tabled::Style::modern().get_horizontal(),
                )]),
        )
        .with(tabled::Disable::row(Rows::first()));
    println!("{}", table);

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_names().await?;
    let cli = Cli::parse();

    match cli.command {
        Commands::Route { route } => {
            search_route_info(&route.to_uppercase(), true).await?;
        }

        Commands::Eta {
            route,
            direction,
            service_type,
        } => {
            search_route_eta(&route.to_uppercase(), &direction, &service_type).await?;
        }
    }

    Ok(())
}
