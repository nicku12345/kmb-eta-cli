use clap::{Parser, Subcommand};
use lazy_static::lazy_static;
use std::{collections::HashMap, sync::Mutex, time::Instant};
use tabled::{locator::ByColumnName, style::HorizontalLine, Alignment, Modify, Table, Tabled};

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(short, long)]
    debug: bool,
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

        /// To query the inbound route.
        /// Exactly one of `inbound` or `outbound` is to be set `true`
        #[arg(short, long)]
        inbound: bool,

        /// To query the outbound route.
        /// Exactly one of `inbound` or `outbound` is to be set `true`
        #[arg(short, long)]
        outbound: bool,

        /// Route service type
        #[arg(short, long, default_value = "1")]
        service_type: i64,
    },

    /// Display all route info. Example `kmb-eta-cli all | fzf`
    All,
}

#[derive(Tabled, Clone)]
struct RouteInfo {
    route: String,
    service_type: i64,
    direction: String,
    orig: String,
    dest: String,
}

#[derive(Tabled, Clone)]
struct StopIdName {
    stop_id: String,
    stop_name: String,
}

#[derive(Tabled, Clone)]
struct RouteEtaInfo {
    seq: String,
    stop_name: String,
    t1: String,
    t2: String,
    t3: String,
}

struct HKGovAPI {}
impl HKGovAPI {
    const BASE_URL: &str = "https://data.etabus.gov.hk";
    const STOP_URL: &str = "v1/transport/kmb/stop";
    const ROUTE_STOP_URL: &str = "v1/transport/kmb/route-stop";
    const ROUTE_ETA_URL: &str = "v1/transport/kmb/route-eta";
    const ROUTE_URL: &str = "v1/transport/kmb/route";
}

lazy_static!(
    // use singleton reqwest client instead of calling reqwest::get(...)
    // massive performance boost
    static ref REQWEST_CLIENT: reqwest::Client = reqwest::Client::new();

    // key: route, value: vec of route infos
    static ref ROUTES: Mutex<HashMap<String, Vec<RouteInfo>>> = Mutex::new(HashMap::new());

    // key: stop_id, value: stopIdName struct
    static ref STOP_ID_NAMES: Mutex<HashMap<String, StopIdName>> = Mutex::new(HashMap::new());
);

async fn load_names() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let req_url = format!("{}/{}", HKGovAPI::BASE_URL, HKGovAPI::STOP_URL);

    let body = REQWEST_CLIENT
        .get(req_url)
        .send()
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

            mutext_stop_id_names.insert(
                stop_id.to_string(),
                StopIdName {
                    stop_id: stop_id.to_string(),
                    stop_name: name_tc.to_string(),
                },
            );
        });

    Ok(())
}

async fn search_route_eta(
    route: &str,
    direction: &str,
    service_type: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    // make sure route exists
    search_route_info(route, false, Some(direction), Some(service_type)).await?;

    let t_route = route.to_string();
    let t_direction = direction.to_string();
    let t_service_type = service_type;
    let task_route_ids = tokio::spawn(async move {
        let req_url = format!(
            "{}/{}/{}/{}/{}",
            HKGovAPI::BASE_URL,
            HKGovAPI::ROUTE_STOP_URL,
            t_route,
            t_direction,
            t_service_type
        );

        let body = REQWEST_CLIENT
            .get(req_url)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        Ok::<serde_json::Value, Box<dyn std::error::Error + Send + Sync>>(body)
    });

    let t_route = route.to_string();
    let t_service_type = service_type;
    let task_stop_eta = tokio::spawn(async move {
        let req_url = format!(
            "{}/{}/{}/{}",
            HKGovAPI::BASE_URL,
            HKGovAPI::ROUTE_ETA_URL,
            t_route,
            t_service_type
        );

        let body = REQWEST_CLIENT
            .get(req_url)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        Ok::<serde_json::Value, Box<dyn std::error::Error + Send + Sync>>(body)
    });

    let body_route_ids = task_route_ids.await?.unwrap();
    let body_stop_eta = task_stop_eta.await?.unwrap();

    let route_ids = body_route_ids["data"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .fold(vec![], |mut cur, data| {
            let seq = data["seq"].as_str().unwrap().parse::<i64>().unwrap();
            let stop_id = data["stop"].as_str().unwrap();

            cur.push((seq, String::from(stop_id)));
            cur
        });

    let mut stop_eta = HashMap::new();
    let parse_timestmap =
        |timestamp_val: &serde_json::Value| -> Result<i64, Box<dyn std::error::Error>> {
            if timestamp_val.is_string() {
                let timestamp_str = timestamp_val.as_str().unwrap();
                let rfc3339 = chrono::DateTime::parse_from_rfc3339(timestamp_str);
                if let Ok(t) = rfc3339 {
                    return Ok(t.timestamp());
                }
            }
            Err(format!(
                "Failed to parse timestamp string {}",
                timestamp_val
            ))?
        };

    let api_timestamp = parse_timestmap(&body_stop_eta["generated_timestamp"])?;

    body_stop_eta["data"]
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
            let eta_timestamp = parse_timestmap(&data["eta"]);

            let eta_repr = match eta_timestamp {
                Ok(t) => {
                    let eta_diff = t - api_timestamp;
                    if eta_diff > 0 {
                        // spare 3 chars for minutes, 2 chars for seconds
                        format!("{:>3}m {:>2}s", eta_diff / 60, eta_diff % 60,)
                    } else {
                        "LEAVING".to_string()
                    }
                }
                Err(_) => "".to_string(),
            };

            stop_eta.insert((seq, eta_seq), eta_repr);
        });

    let mutex_stop_id_names = STOP_ID_NAMES.lock().unwrap();
    let mut output = vec![];

    let empty_eta = &"".to_string();
    for (ref_seq, stop_id) in &route_ids {
        let seq = *ref_seq;

        let stop_name = &mutex_stop_id_names.get(stop_id).unwrap().stop_name;

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
    table
        .with(
            tabled::Style::modern()
                .off_horizontal()
                .horizontals([HorizontalLine::new(
                    1,
                    tabled::Style::modern().get_horizontal(),
                )]),
        )
        .with(Modify::new(ByColumnName::new("t1")).with(Alignment::right()))
        .with(Modify::new(ByColumnName::new("t2")).with(Alignment::right()))
        .with(Modify::new(ByColumnName::new("t3")).with(Alignment::right()));

    println!("{}", table);

    Ok(())
}

async fn load_routes() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let req_url = format!("{}/{}", HKGovAPI::BASE_URL, HKGovAPI::ROUTE_URL,);

    let body = REQWEST_CLIENT
        .get(req_url)
        .send()
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

        if let Some(route_infos) = mutex_routes.get_mut(route) {
            route_infos.push(RouteInfo {
                route: route.to_string(),
                service_type,
                direction: bound.to_string(),
                orig: orig_tc.to_string(),
                dest: dest_tc.to_string(),
            });
        } else {
            mutex_routes.insert(
                route.to_string(),
                vec![RouteInfo {
                    route: route.to_string(),
                    service_type,
                    direction: bound.to_string(),
                    orig: orig_tc.to_string(),
                    dest: dest_tc.to_string(),
                }],
            );
        }
    });

    Ok(())
}

async fn search_route_info(
    route: &str,
    to_print: bool,
    direction: Option<&str>,
    service_type: Option<i64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mutex_routes = ROUTES.lock().unwrap();

    let mut route_info = mutex_routes.get(route).unwrap_or(&vec![]).clone();

    if let (Some(d), Some(t)) = (direction, service_type) {
        route_info.retain(|r| r.direction == d && r.service_type == t);
    }

    if route_info.is_empty() {
        let err_msg;
        if let (Some(d), Some(t)) = (direction, service_type) {
            err_msg = format!(
                "(route: {}, direction: {}, service_type: {}) does not exist!",
                route, d, t
            );
        } else {
            err_msg = format!("(route: {}) does not exist!", route);
        }
        Err(err_msg)?;
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

fn search_all_route_info() {
    let mutex_routes = ROUTES.lock().unwrap();
    let all_route_info = mutex_routes
        .iter()
        .fold(vec![], |mut cur, (_, route_infos)| {
            cur.append(&mut route_infos.clone());
            cur
        });

    let mut table = Table::new(all_route_info);
    table.with(
        tabled::Style::modern()
            .off_horizontal()
            .off_top()
            .off_bottom(),
    );

    println!("{}", table);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let task_load_names = tokio::spawn(async move {
        load_names().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    let task_load_routes = tokio::spawn(async move {
        load_routes().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    task_load_names.await?.expect("failed to load stop names!");
    task_load_routes.await?.expect("failed to load routes!");

    let start = Instant::now();

    match cli.command {
        Commands::Route { route } => {
            search_route_info(&route.to_uppercase(), true, None, None).await?;
        }

        Commands::Eta {
            route,
            inbound,
            outbound,
            service_type,
        } => {
            if inbound == outbound {
                Err("Please set exactly one of `inbound` or `outbound` to `true`")?;
            }

            let direction = match inbound {
                true => "inbound",
                false => {
                    assert!(outbound);
                    "outbound"
                }
            };

            search_route_eta(&route.to_uppercase(), direction, service_type).await?;
        }

        Commands::All => {
            search_all_route_info();
        }
    }

    if cli.debug {
        println!("time elapsed: {:?}", start.elapsed());
    }

    Ok(())
}
