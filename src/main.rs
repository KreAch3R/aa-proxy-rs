mod bluetooth;
mod io_uring;
mod usb_gadget;

use bluer::Address;
use bluetooth::bluetooth_setup_connection;
use bluetooth::bluetooth_stop;
use clap::Parser;
use humantime::format_duration;
use io_uring::io_loop;
use simplelog::*;
use usb_gadget::uevent_listener;
use usb_gadget::UsbGadgetState;

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Builder;
use tokio::sync::Notify;
use tokio::time::Instant;

// module name for logging engine
const NAME: &str = "<i><bright-black> main: </>";

const TCP_SERVER_PORT: i32 = 5288;

/// AndroidAuto wired/wireless proxy
#[derive(Parser, Debug)]
#[clap(version, about, long_about = None)]
struct Args {
    /// BLE advertising
    #[clap(short, long)]
    advertise: bool,

    /// Enable debug info
    #[clap(short, long)]
    debug: bool,

    /// Enable legacy mode
    #[clap(short, long)]
    legacy: bool,

    /// Auto-connect to saved phone or specified phone MAC address if provided
    #[clap(short, long, default_missing_value("00:00:00:00:00:00"))]
    connect: Option<Address>,

    /// Log file path
    #[clap(
        short,
        long,
        parse(from_os_str),
        default_value = "/var/log/aa-proxy-rs.log"
    )]
    logfile: PathBuf,

    /// Interval of showing data transfer statistics (0 = disabled)
    #[clap(short, long, value_name = "SECONDS", default_value_t = 0)]
    stats_interval: u16,

    /// UDC Controller name
    #[clap(
        short,
        long
    )]
    udc: Option<String>,

    /// WLAN / Wi-Fi Hotspot interface
    #[clap(
        short,
        long,
        default_value = "wlan0"
    )]
    iface: String,
    
}

fn logging_init(debug: bool, log_path: &PathBuf) {
    let conf = ConfigBuilder::new()
        .set_time_format("%F, %H:%M:%S%.3f".to_string())
        .set_write_log_enable_colors(true)
        .build();

    let mut loggers = vec![];

    let requested_level = if debug {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };

    let console_logger: Box<dyn SharedLogger> = TermLogger::new(
        requested_level,
        conf.clone(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    );
    loggers.push(console_logger);

    let mut logfile_error: Option<String> = None;
    let logfile = OpenOptions::new().create(true).append(true).open(&log_path);
    match logfile {
        Ok(logfile) => {
            loggers.push(WriteLogger::new(requested_level, conf, logfile));
        }
        Err(e) => {
            logfile_error = Some(format!(
                "Error creating/opening log file: {:?}: {:?}",
                log_path, e
            ));
        }
    }

    CombinedLogger::init(loggers).expect("Cannot initialize logging subsystem");
    if logfile_error.is_some() {
        error!("{} {}", NAME, logfile_error.unwrap());
        warn!("{} Will do console logging only...", NAME);
    }
}

async fn tokio_main(
    advertise: bool,
    legacy: bool,
    iface: String,
    connect: Option<Address>,
    udc: Option<String>,
    need_restart: Arc<Notify>,
    tcp_start: Arc<Notify>,
) {
    let accessory_started = Arc::new(Notify::new());
    let accessory_started_cloned = accessory_started.clone();

    if legacy {
        // start uevent listener in own task
        std::thread::spawn(|| uevent_listener(accessory_started_cloned));
    }

    let mut usb = UsbGadgetState::new(legacy, udc);
    loop {
        if let Err(e) = usb.init() {
            error!("{} 🔌 USB init error: {}", NAME, e);
        }

        let bt_stop;
        loop {
            match bluetooth_setup_connection(advertise, &iface, connect, tcp_start.clone()).await {
                Ok(state) => {
                    // we're ready, gracefully shutdown bluetooth in task
                    bt_stop = tokio::spawn(async move { bluetooth_stop(state).await });
                    break;
                }
                Err(e) => {
                    error!("{} Bluetooth error: {}", NAME, e);
                    info!("{} Trying to recover...", NAME);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }

        usb.enable_default_and_wait_for_accessory(accessory_started.clone())
            .await;

        // wait for bluetooth stop properly
        let _ = bt_stop.await;

        // wait for restart
        need_restart.notified().await;

        // TODO: make proper main loop with cancelation
        info!(
            "{} 📵 TCP/USB connection closed or not started, trying again...",
            NAME
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn main() {
    let started = Instant::now();
    let args = Args::parse();
    logging_init(args.debug, &args.logfile);

    let stats_interval = {
        if args.stats_interval == 0 {
            None
        } else {
            Some(Duration::from_secs(args.stats_interval.into()))
        }
    };

    info!(
        "🛸 <b><blue>aa-proxy-rs</> is starting, build: {}, git: {}-{}",
        env!("BUILD_DATE"),
        env!("GIT_DATE"),
        env!("GIT_HASH")
    );
    info!(
        "{} 📜 Log file path: <b><green>{}</>",
        NAME,
        args.logfile.display()
    );
    info!(
        "{} ⚙️ Showing transfer statistics: <b><blue>{}</>",
        NAME,
        match stats_interval {
            Some(d) => format_duration(d).to_string(),
            None => "disabled".to_string(),
        }
    );

    // notify for syncing threads
    let need_restart = Arc::new(Notify::new());
    let need_restart_cloned = need_restart.clone();
    let tcp_start = Arc::new(Notify::new());
    let tcp_start_cloned = tcp_start.clone();

    // build and spawn main tokio runtime
    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.spawn(async move {
        tokio_main(
            args.advertise,
            args.legacy,
            args.iface,
            args.connect,
            args.udc,
            need_restart,
            tcp_start,
        )
        .await
    });

    // start tokio_uring runtime simultaneously
    let _ = tokio_uring::start(io_loop(
        stats_interval,
        need_restart_cloned,
        tcp_start_cloned,
    ));

    info!(
        "🚩 aa-proxy-rs terminated, running time: {}",
        format_duration(started.elapsed()).to_string()
    );
}
