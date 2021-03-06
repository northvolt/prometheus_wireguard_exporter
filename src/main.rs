extern crate serde_json;
#[macro_use]
extern crate failure;
use clap::{crate_authors, crate_name, crate_version, Arg};
use hyper::{Body, Request};
use log::{debug, info, trace};
use std::env;
mod options;
use options::Options;
mod wireguard;
use std::convert::TryFrom;
use std::process::Command;
use std::string::String;
use wireguard::WireGuard;
mod exporter_error;
mod wireguard_config;
use wireguard_config::peer_entry_hashmap_try_from;
extern crate prometheus_exporter_base;
use prometheus_exporter_base::render_prometheus;
use std::net::IpAddr;
use std::sync::Arc;

async fn perform_request(
    _req: Request<Body>,
    options: Arc<Options>,
) -> Result<String, failure::Error> {
    let interfaces_to_handle = match &options.interfaces {
        Some(interfaces_str) => interfaces_str.clone(),
        None => vec!["all".to_owned()],
    };

    let peer_entry_contents =
        if let Some(extract_names_config_file) = &options.extract_names_config_file {
            Some(::std::fs::read_to_string(
                &extract_names_config_file as &str,
            )?)
        } else {
            None
        };

    let peer_entry_hashmap = if let Some(peer_entry_contents) = &peer_entry_contents {
        Some(peer_entry_hashmap_try_from(peer_entry_contents)?)
    } else {
        None
    };

    let mut wg_accumulator: Option<WireGuard> = None;

    for interface_to_handle in interfaces_to_handle {
        let output = Command::new("wg")
            .arg("show")
            .arg(&interface_to_handle)
            .arg("dump")
            .output()?;
        let output_stdout_str = String::from_utf8(output.stdout)?;
        trace!(
            "wg show {} dump stdout == {}",
            interface_to_handle,
            output_stdout_str
        );
        let output_stderr_str = String::from_utf8(output.stderr)?;
        trace!(
            "wg show {} dump stderr == {}",
            interface_to_handle,
            output_stderr_str
        );

        // the output of wg show is different if we use all or we specify an interface.
        // In the first case the first column will be the interface name. In the second case
        // the interface name will be omitted. We need to compensate for the skew somehow (one
        // column less in the second case). We solve this prepending the interface name in every
        // line so the output of the second case will be equal to the first case.
        let output_stdout_str = if interface_to_handle != "all" {
            debug!("injecting {} to the wg show output", interface_to_handle);
            let mut result = String::new();
            for s in output_stdout_str.lines() {
                result.push_str(&format!("{}\t{}\n", interface_to_handle, s));
            }
            result
        } else {
            output_stdout_str
        };

        if let Some(wg_accumulator) = &mut wg_accumulator {
            let wg = WireGuard::try_from(&output_stdout_str as &str)?;
            wg_accumulator.merge(&wg);
        } else {
            wg_accumulator = Some(WireGuard::try_from(&output_stdout_str as &str)?);
        };
    }

    if let Some(wg_accumulator) = wg_accumulator {
        Ok(wg_accumulator.render_with_names(
            peer_entry_hashmap.as_ref(),
            options.separate_allowed_ips,
            options.export_remote_ip_and_port,
        ))
    } else {
        panic!();
    }
}

#[tokio::main]
async fn main() {
    let matches = clap::App::new(crate_name!())
        .version(crate_version!())
        .author(crate_authors!("\n"))
        .arg(
            Arg::with_name("addr")
                .short("l")
                .help("exporter address")
                .default_value("0.0.0.0")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("port")
                .short("p")
                .help("exporter port")
                .default_value("9586")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("verbose")
                .short("v")
                .help("verbose logging")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("separate_allowed_ips")
                .short("s")
                .help("separate allowed ips and ports")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("export_remote_ip_and_port")
                .short("r")
                .help("exports peer's remote ip and port as labels (if available)")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("extract_names_config_files")
                .short("n")
                .help("If set, the exporter will look in the specified WireGuard config file for peer names (must be in [Peer] definition and be a comment)")
                .multiple(false)
                .number_of_values(1)
                .takes_value(true))
        .arg(
            Arg::with_name("interfaces")
                .short("i")
                .help("If set specifies the interface passed to the wg show command. It is relative to the same position config_file. In not specified, all will be passed.")
                .multiple(true)
                .number_of_values(1)
                .takes_value(true))
        .get_matches();

    let options = Options::from_claps(&matches);

    if options.verbose {
        env::set_var(
            "RUST_LOG",
            format!("{}=trace,prometheus_exporter_base=trace", crate_name!()),
        );
    } else {
        env::set_var(
            "RUST_LOG",
            format!("{}=info,prometheus_exporter_base=info", crate_name!()),
        );
    }
    env_logger::init();

    info!(
        "{} v{} starting...",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );
    info!("using options: {:?}", options);

    let bind = matches.value_of("port").unwrap();
    let bind = u16::from_str_radix(&bind, 10).expect("port must be a valid number");
    let ip = matches.value_of("addr").unwrap().parse::<IpAddr>().unwrap();
    let addr = (ip, bind).into();

    info!("starting exporter on http://{}/metrics", addr);

    render_prometheus(addr, options, |request, options| {
        Box::pin(perform_request(request, options))
    })
    .await;
}
