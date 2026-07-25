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
const LIGHT_GAP: Duration = Duration::from_millis(5);
const ACK_TIMEOUT: Duration = Duration::from_millis(1500);

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
    Light(LightCmd),
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

/// control the lighting
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "light")]
struct LightCmd {
    #[argh(subcommand)]
    what: LightWhat,
}

#[derive(argh::FromArgs)]
#[argh(subcommand)]
enum LightWhat {
    Off(LightOffCmd),
    Effect(LightEffectCmd),
    Static(LightStaticCmd),
    Gear(LightGearCmd),
}

/// paint the strip a single colour
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "static")]
struct LightStaticCmd {
    /// colour as rrggbb, with or without a leading #
    #[argh(positional)]
    color: String,

    /// brightness percentage, 0-100 (default: 100)
    #[argh(option, short = 'b', default = "100")]
    brightness: u8,
}

/// turn the RGB strip off
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "off")]
struct LightOffCmd {}

/// play one of the firmware's built-in effects
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "effect")]
struct LightEffectCmd {
    /// effect number, 1-5 (0 replays the uploaded colour)
    #[argh(positional)]
    mode: u8,
}

/// toggle the gear indicator LEDs
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "gear")]
struct LightGearCmd {
    /// on or off
    #[argh(positional)]
    state: String,
}

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

fn parse_color(text: &str) -> Result<protocol::Rgb> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 {
        return Err(Error::BadColor(text.to_string()));
    }

    let byte = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16).map_err(|_| Error::BadColor(text.to_string()))
    };

    Ok(protocol::Rgb {
        r: byte(0..2)?,
        g: byte(2..4)?,
        b: byte(4..6)?,
    })
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

            dev.send_acked(protocol::enter_realtime(), ACK_TIMEOUT)?;
            std::thread::sleep(Duration::from_millis(300));
            dev.send_acked(protocol::set_realtime_rpm(cmd.rpm), ACK_TIMEOUT)?;

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
            dev.send_acked(protocol::exit_realtime(), ACK_TIMEOUT)?;
            info!("released to gear mode");
        }

        Command::Light(cmd) => match cmd.what {
            LightWhat::Off(_) => {
                dev.send_acked(protocol::light_off(), ACK_TIMEOUT)?;
                info!("strip off");
            }

            LightWhat::Effect(cmd) => {
                if cmd.mode > protocol::MAX_EFFECT {
                    return Err(Error::UnknownEffect {
                        mode: cmd.mode,
                        max: protocol::MAX_EFFECT,
                    });
                }

                // The firmware gates built-in effects behind realtime mode and
                // still acknowledges them otherwise, so check before promising
                // anything.
                if cmd.mode != 0 {
                    let status = dev.read_status(READ_TIMEOUT)?;
                    if status.mode != protocol::Mode::Realtime {
                        return Err(Error::NeedsRealtime);
                    }
                }

                for report in protocol::light_effect(cmd.mode) {
                    dev.send_acked(report, ACK_TIMEOUT)?;
                    std::thread::sleep(LIGHT_GAP);
                }

                if cmd.mode == 0 {
                    info!("strip on uploaded buffer");
                } else {
                    info!("effect {}", cmd.mode);
                }
            }

            LightWhat::Static(cmd) => {
                let color = parse_color(&cmd.color)?;
                for report in protocol::light_static(color, cmd.brightness) {
                    dev.send_acked(report, ACK_TIMEOUT)?;
                    std::thread::sleep(LIGHT_GAP);
                }
                info!(
                    "colour #{:02x}{:02x}{:02x} at {}%",
                    color.r, color.g, color.b, cmd.brightness
                );
            }

            LightWhat::Gear(gear) => {
                let on = match gear.state.as_str() {
                    "on" | "true" | "1" => true,
                    "off" | "false" | "0" => false,
                    other => return Err(Error::BadArgument(other.to_string())),
                };
                dev.send_acked(protocol::gear_light(on), ACK_TIMEOUT)?;
                info!("gear LEDs {}", if on { "on" } else { "off" });
            }
        },
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
