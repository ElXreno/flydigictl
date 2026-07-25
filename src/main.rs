mod device;
mod error;
mod protocol;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use log::{error, info, warn};

use device::Device;
use error::{Error, Result};
use protocol::{MAX_RPM, MIN_RPM};

const READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Control Flydigi BS series laptop coolers
#[derive(argh::FromArgs)]
struct Args {
    /// hidraw device path (auto-detected if omitted)
    #[argh(option, short = 'd')]
    device: Option<PathBuf>,

    #[argh(subcommand)]
    command: Command,
}

#[derive(argh::FromArgs)]
#[argh(subcommand)]
enum Command {
    List(ListCmd),
    Status(StatusCmd),
    Watch(WatchCmd),
    Set(SetCmd),
    Auto(AutoCmd),
}

/// list detected coolers
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "list")]
struct ListCmd {}

/// print one status frame
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "status")]
struct StatusCmd {}

/// stream status frames until interrupted
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "watch")]
struct WatchCmd {
    /// stop after this many frames (default: unlimited)
    #[argh(option, short = 'n')]
    count: Option<usize>,
}

/// hold a fixed fan speed
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "set")]
struct SetCmd {
    /// target speed in RPM
    #[argh(positional)]
    rpm: u16,
}

/// release the fixed speed and return to the selected gear
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "auto")]
struct AutoCmd {}

/// Retry until the cooler shows up again; it re-enumerates a couple of seconds
/// after a power blip.
fn reopen(path: Option<&std::path::Path>) -> Result<Device> {
    loop {
        match Device::open(path) {
            Ok(dev) => return Ok(dev),
            Err(Error::NotFound) | Err(Error::Open { .. }) => {
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(err) => return Err(err),
        }
    }
}

fn print_status(status: &protocol::Status) {
    println!(
        "current {:4} rpm   target {:4} rpm   mode {:8}   gear {} (max {})",
        status.current_rpm, status.target_rpm, status.mode, status.gear, status.max_gear
    );
}

fn run() -> Result<()> {
    let args: Args = argh::from_env();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    if let Command::List(_) = args.command {
        let found = device::find_all();
        if found.is_empty() {
            return Err(Error::NotFound);
        }
        for dev in found {
            println!("{}  {}", dev.path.display(), dev.model.name());
        }
        return Ok(());
    }

    let mut dev = Device::open(args.device.as_deref())?;
    info!("{} on {}", dev.model.name(), dev.path.display());

    match args.command {
        Command::List(_) => unreachable!("handled above"),

        Command::Status(_) => print_status(&dev.read_status(READ_TIMEOUT)?),

        Command::Watch(cmd) => {
            let mut seen = 0usize;
            loop {
                match dev.read_status(READ_TIMEOUT) {
                    Ok(status) => {
                        print_status(&status);
                        seen += 1;
                        if cmd.count.is_some_and(|limit| seen >= limit) {
                            break;
                        }
                    }

                    // The cooler goes quiet whenever it loses power, and the
                    // Bluetooth link only drops a few seconds later, so a gap
                    // in the stream is not a reason to give up.
                    Err(Error::Timeout) => {
                        warn!("no frames for {}s", READ_TIMEOUT.as_secs());
                    }

                    // Reconnecting builds a fresh HID device that reuses the
                    // old name, so the previous descriptor is dead: reopen.
                    Err(Error::Disconnected) => {
                        warn!("device went away, waiting");
                        dev = reopen(args.device.as_deref())?;
                        info!("reconnected on {}", dev.path.display());
                    }

                    Err(err) => return Err(err),
                }
            }
        }

        Command::Set(cmd) => {
            let stopping = cmd.rpm == protocol::STOP_RPM;
            if !stopping && !(MIN_RPM..=MAX_RPM).contains(&cmd.rpm) {
                return Err(Error::RpmOutOfRange {
                    rpm: cmd.rpm,
                    min: MIN_RPM,
                    max: MAX_RPM,
                });
            }

            dev.send(protocol::enter_realtime())?;
            std::thread::sleep(Duration::from_millis(300));
            dev.send(protocol::set_realtime_rpm(cmd.rpm))?;

            if stopping {
                info!("fan stopped");
            } else {
                info!(
                    "target {} rpm",
                    cmd.rpm
                );
            }
        }

        Command::Auto(_) => {
            dev.send(protocol::exit_realtime())?;
            info!("released to gear mode");
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err}");
            ExitCode::FAILURE
        }
    }
}
