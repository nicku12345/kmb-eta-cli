use clap::{Parser, Subcommand};
use std::{collections::HashMap, sync::Mutex};

use tabled::{style::HorizontalLine, Table, Tabled};

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

#[derive(Tabled, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RouteInfo {
    route: String,
    service_type: i64,
    direction: String,
    orig: String,
    dest: String,
}

#[derive(Tabled, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StopIdName {
    stop_id: String,
    stop_name: String,
}

#[derive(Tabled, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RouteEtaInfo {
    seq: String,
    stop_name: String,
    t1: String,
    t2: String,
    t3: String,
}

const BASE_URL: &str = "https://data.etabus.gov.hk";
static ROUTES: Mutex<Vec<RouteInfo>> = Mutex::new(Vec::new());
static STOP_ID_NAMES: Mutex<Vec<StopIdName>> = Mutex::new(Vec::new());

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

            mutext_stop_id_names.push(StopIdName {
                stop_id: stop_id.to_string(),
                stop_name: name_tc.to_string(),
            })
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
    let mut output = vec![];

    let empty_eta = &"".to_string();
    for (ref_seq, stop_id) in &route_ids {
        let seq = *ref_seq;
        let query = StopIdName {
            stop_id: stop_id.to_string(),
            stop_name: String::new(),
        };

        let idx = match mutex_stop_id_names.binary_search(&query) {
            Ok(i) => i,
            Err(i) => i,
        };

        let stop_name = &mutex_stop_id_names.get(idx).unwrap().stop_name;

        let first_eta = stop_eta.get(&(seq, 1)).unwrap_or(empty_eta);
        let second_eta = stop_eta.get(&(seq, 2)).unwrap_or(empty_eta);
        let third_eta = stop_eta.get(&(seq, 3)).unwrap_or(empty_eta);

        output.push(RouteEtaInfo {
            seq: seq.to_string(),
            stop_name: stop_name.to_string(),
            t1: first_eta.to_string(),
            t2: second_eta.to_string(),
            t3: third_eta.to_string(),
        })
        // output.push((seq.to_string(), stop_name, first_eta, second_eta, third_eta));
    }

    let mut table = Table::new(output);
    table.with(
        tabled::Style::modern()
            .off_horizontal()
            .horizontals([HorizontalLine::new(
                1,
                tabled::Style::modern().get_horizontal(),
            )]),
    );
    println!("{}", table);

    Ok(())
}

async fn load_routes() -> Result<(), Box<dyn std::error::Error>> {
    let api_url = "v1/transport/kmb/route";
    let req_url = format!("{}/{}", BASE_URL, api_url,);

    let body = reqwest::get(req_url)
        .await?
        .json::<serde_json::Value>()
        .await?;

    let mut mutex_routes = ROUTES.lock().unwrap();
    body["data"].as_array().unwrap().iter().for_each(|data| {
        let route = data["route"].as_str().unwrap();
        let service_type = data["service_type"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let orig_tc = data["orig_tc"].as_str().unwrap();
        let dest_tc = data["dest_tc"].as_str().unwrap();
        let bound = match data["bound"].as_str().unwrap() {
            "O" => "outbound",
            "I" => "inbound",
            _ => "",
        };

        mutex_routes.push(RouteInfo {
            route: route.to_string(),
            service_type,
            direction: bound.to_string(),
            orig: orig_tc.to_string(),
            dest: dest_tc.to_string(),
        })
    });

    mutex_routes.sort();
    Ok(())
}

async fn search_route_info(route: &str, to_print: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mutex_routes = ROUTES.lock().unwrap();

    let query = RouteInfo {
        route: route.to_string(),
        service_type: 0,
        direction: "".to_string(),
        orig: "".to_string(),
        dest: "".to_string(),
    };

    let mut idx = match mutex_routes.binary_search(&query) {
        Ok(i) => i,
        Err(i) => i,
    };

    let mut route_info = vec![];
    while idx < mutex_routes.len() {
        let r = mutex_routes.get(idx).unwrap();
        if r.route != route {
            break;
        }

        route_info.push(r.clone());
        idx += 1;
    }

    if route_info.is_empty() {
        Err("route does not exist")?;
    }

    if !to_print {
        return Ok(());
    }

    let mut table = Table::new(route_info);
    table.with(
        tabled::Style::modern()
            .off_horizontal()
            .horizontals([HorizontalLine::new(
                1,
                tabled::Style::modern().get_horizontal(),
            )]),
    );
    println!("{}", table);

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_names().await?;
    load_routes().await?;

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
