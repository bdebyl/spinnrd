//! # spinnrd
//!
//! `spinnrd`, the spinnr daemon, translates accelerometer output
//! into screen orientation.

#[macro_use] extern crate lazy_static;
extern crate daemonize;
extern crate simplelog;
extern crate chrono;
extern crate signal;
extern crate syslog;
// extern crate errno;
extern crate clap;
// extern crate libc;
#[macro_use] extern crate log;

#[cfg(feature = "sysd")]
extern crate systemd;

// For fs-accel
#[cfg(feature = "fsaccel")]
extern crate regex;
#[cfg(feature = "fsaccel")]
extern crate glob;

mod accel;


use accel::{Accelerometer, FilteredAccelerometer};
#[cfg(feature = "fsaccel")]
use accel::fsaccel::FsAccelerometer;

use std::time::{Duration, Instant};
use std::thread::sleep;
use std::thread;
use std::sync::mpsc;
use std::fs::{File,remove_file,OpenOptions};
// use std::ffi::CString;
// use std::os::unix::io::AsRawFd;
use std::io::Write;
use std::path::{PathBuf};
// use std::io::Error as IoError;
// use std::io::ErrorKind as IoErrorKind;
// use std::io::SeekFrom;
use std::fmt::{Display, Formatter};
use std::fmt::Result as FmtResult;

use daemonize::Daemonize;
use clap::{Arg,ArgMatches};
use signal::trap::Trap;
use signal::Signal;
// use chrono::{DateTime, Local};
// use errno::{errno,Errno};
use log::{LevelFilter,SetLoggerError};
use simplelog::WriteLogger;
// use libc::{pid_t, fork, setsid, chdir, close};
// use libc::{getpid, fcntl, flock, F_SETLK, SEEK_SET};

#[cfg(feature = "sysd")]
use systemd::journal::JournalLog;

// pub const F_RDLCK: ::libc::c_short = 1;

/// The default interval between accelerometer polls (in ms)
/// Note that this must stay in the same units as the hysteresis!
const DEFAULT_PERIOD: u32   = 150;

/// Multiply by the period to get nanoseconds
const PERIOD_NS_MULT: u32   = 1000000;

/// Divide the period by this to get seconds
const PERIOD_SEC_DIV: u32   = 1000;

/// The default amount of time we're filtering over (in ms)
/// Note that this must stay in the same units as the period!
const DEFAULT_HYSTERESIS: u32   = 1000;

// This helps filter out transitional rotations, i.e. when turning the screen 180°
/// The default delay before committing an orientation change (in ms)
const DEFAULT_DELAY: u32    = 350;

/// Multiply by the delay to get nanoseconds
const DELAY_NS_MULT: u32    = 1000000;

/// Divide the delay by this to get seconds
const DELAY_SEC_DIV: u32   = 1000;

/// The default pid file
const DEFAULT_PID_FILE: &'static str = "/run/spinnr.pid";

/// The backup pid file
const BACKUP_PID_FILE: &'static str = "/tmp/spinnr.pid";

/// The default logging level
#[cfg(debug_assertions)]
const DEFAULT_LOG_LEVEL: log::LevelFilter = log::LevelFilter::Debug;
#[cfg(not(debug_assertions))]
const DEFAULT_LOG_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// The default logfile
const DEFAULT_LOG_FILE: &'static str = "syslog";

/// The file to (try) to write the logging fail message to
const LOG_FAIL_FILE: &'static str = "/tmp/spinnr.%t.logfail";

// the part where we define the command line arguments
lazy_static!{
    /// The command line arguments
    static ref CLI_ARGS: ArgMatches<'static> = clap::App::new("Spinnr")

        .version("1.0.0")
        .author("James Wescott <james@wescottdesign.com>")
        .about("Automatically rotates display and touchscreens based on accelerometer data")
        .arg(Arg::with_name("quiet")
            .short("q")
            .long("quiet")
            .help("Turns off printing to stdout")
            )
        .arg(Arg::with_name("interval")
             .long("interval")
             .short("i")
            .validator(validate_u32)
             .help("Set the polling interval in milliseconds")
             .value_name("INTERVAL")
             )
        .arg(Arg::with_name("hysteresis")
             .long("hysteresis")
             .short("H")
            .validator(validate_u32)
             .help("How long to average the accelerometer inputs over, in milliseconds")
             .value_name("HYSTERESIS")
             )
        .arg(Arg::with_name("wait")
            .short("w")
            .long("wait")
            .value_name("TIME")
            // .default_value(DEFAULT_WAIT_SECONDS)
            .validator(validate_u32)
            .help("Wait TIME seconds before starting")
            .empty_values(true)
            )
        .arg(Arg::with_name("pidfile")
             .long("pid-file")
             .value_name("FILE")
             // .default_value(DEFAULT_PID_FILE)
             .help("Location of the pid file (if daemonizing)")
             )
        .arg(Arg::with_name("nopidfile")
             .long("no-pid-file")
             .help("Don't make a pid file")
             )
        .arg(Arg::with_name("daemonize")
             .short("D")
             .long("daemonize")
             .help("Run as background daemon.")
             )
        .arg(Arg::with_name("delay")
             .long("delay")
             .short("d")
             .value_name("DELAY")
             .validator(validate_u32)
             .help("Wait for orientation to be stable for DELAY milliseconds before rotating display.")
             )
        .get_matches();
}


fn main() {
    // lets us exit with status - important for running under systemd, etc.
    ::std::process::exit(mainprog());
}

/// The actual main body of the program
fn mainprog() -> i32 {
    match init_logger() {
        Ok(l)   => {
            if ! is_quiet() {
                println!("Logging initialized to {}", l);
            }
            debug!("Logging initialized to {}", l);
        },
        Err(e)  => {
            if ! is_quiet() {
                eprintln!("{}", e);
            }
            log_logging_failure(e);
            return 2i32
        }
    }

    let rval;
    let mut pidfile: Option<PathBuf> = None;
    if is_daemon() {
        info!("Attempting abyssal arachnid generation...");
        pidfile = Some(get_pid_file());
        let daemon = Daemonize::new()
            .pid_file(pidfile.unwrap())
            .chown_pid_file(true)
            .working_directory(get_working_dir())
            .user(get_user())
            .group(get_group())
            .umask(0o023)
            ;
        //FEEP: Use socket to communicate (maybe). Or pipe file?

        /*
         * match daemonize() {
         *     Daemonized::Error(e)    => {
         *         //log error
         *         match e {
         *             DaemonizationError::PidFileLock(ref _r, ref _f, ref p)  => {
         *                 rm_pid_file(p);
         *                 warn!("{}. Deleted.",e);
         *             },
         *             DaemonizationError::PidFileWrite(ref _r, ref p) => {
         *                 rm_pid_file(p);
         *                 error!("{}. Deleted; aborting.",e);
         *                 return 1i32;
         *             }
         *             _   => {
         *                 error!("{}. Aborting.",e);
         *                 return 1i32;
         *             }
         *         } // match e
         *     }, // Daemonized::Error(e) =>
         *     Daemonized::Parent(p)   => {
         *         info!("Child forked with PID {}. Exiting...", p);
         *         return 0;
         *     },
         *     Daemonized::Child(f)    => match f {
         *         Some((f,p)) => {
         *             info!("Successfully daemonized with PID-file '{}'", p.display());
         *             _pid_file   = Some(f);
         *             pid_fpath   = Some(p);
         *         },
         *         None    => {
         *             _pid_file   = None;
         *             pid_fpath   = None;
         *             info!("Successfully daemonized with no PID-file");
         *         },
         *     },
         * } // match daemonize()
         */
    } // if is_daemon()


    let hyst = get_u32_arg_val("hysteresis").unwrap_or(DEFAULT_HYSTERESIS);
    let period = get_u32_arg_val("period").unwrap_or(DEFAULT_PERIOD);
    let delay = get_u32_arg_val("delay").unwrap_or(DEFAULT_DELAY);
    // a_now = m * (measurement - a_last)
    // where m is the amount of time we're low-pass filtering over
    // times the frequency with which we're polling
    // (AKA the time we're filtering over divided by the period)
    match init_accel(hyst as f64 / period as f64) {
        Ok(accel,period,delay) => {
            rval = runloop(accel, period, delay);
        },
        Err(e)  => {
            rval = e;
        },
    }

    if let Some(p) = pidfile {
        rm_pid_file(&p)
    }
    return rval;
}

pub fn runloop<O: Orientator>(mut orient: O, period: u32, delay: u32) -> i32 {
    let (handle, sigrx) = init_sigtrap(&[Signal::SIGHUP,Signal::SIGINT,Signal::SIGTERM]);

    let spinfile = get_spinfile();
    // period is in ms, so multiply by 10^6 to get ns
    let period = Duration::new(
        period / PERIOD_SEC_DIV,
        (period % PERIOD_SEC_DIV) * PERIOD_NS_MULT);
    let delay = Duration::new(
        delay / DELAY_SEC_DIV,
        (delay % DELAY_SEC_DIV) * DELAY_NS_MULT);

    let mut orientation: Option<Rotation>;
    let mut last_change: Option<Rotation> = None;
    let mut last_change_time = Instant::now();

    let mut rval = 0;
    info!("Spinning...");
    'mainloop: loop {
        match sigrx.try_recv() {
            Ok(s)   => {
                warn!("\nRecieved {:?}, closing...", s);
                break 'mainloop
            },
            Err(mpsc::TryRecvError::Empty)  => {},
            Err(mpsc::TryRecvError::Disconnected)   => {
                error!("Signal handler died unexpectedly! Aborting!");
                rval = 17;
                break 'mainloop
            },
        } // match sigrx.try_recv()

        if orientation.is_some() {
            if last_change != orientation {
                last_change = orientation;
                last_change_time = Instant::now();
            } else {
                if last_change_time.elapsed() >= delay {
                    // Opening the file every write so inotifywait
                    // is easier (watch for CLOSE_WRITE once instead
                    // of MODIFY twice (open/truncate and write)).
                    match File::create(spinfile) {
                        // unwrap is safe here because we've already checked
                        // that orientation isn't none
                        Ok(f)   => match write!(f, "{}", orientation.unwrap()) {
                            Ok(_)   => (),
                            Err(e)  => {
                                error!("Error writing to spinfile! ({})", e);
                                if quit_on_spinfile_write_error() {
                                    rval = 5;
                                    //FIXME: Do I need to close sigrx?
                                    break 'mainloop
                                }
                            }
                        }, // match write!
                        Err(e)  => {
                            error!("Error opening spinfile! ({})", e);
                            if quit_on_spinfile_open_error() { // This defaults to true!
                                rval = 4;
                                //FIXME: Do I need to close sigrx?
                                break 'mainloop
                            }
                        }
                    } // match File::create(spinfile)
                } // if last_change_time.elapsed() >= delay
            } // if last_change != orientation
        } // if orientation.is_some()
        sleep(period);
    } // 'mainloop: loop
    // unwrapping because it should rejoin nicely
    // and it doesn't matter TOO much if it panics.
    handle.join().unwrap();
    return rval;
}


/// The type returned by init_logger upon failure to initialize a logger.
// #[allow(missing_copy_implementations)]
#[derive(Debug,PartialEq)]
enum LoggingError {
    /// Error parsing the log level
    LogLevel(log::ParseLevelError, String),
    /// Error opening (or writing to?) the log file
    LogFile(std::io::Error, PathBuf),
    /// Error initializing the journald connection
    SystemdDup(SetLoggerError),
    /// Can't initialize journald without systemd functionality
    NoSystemd,
    /// Error initializing the syslog connection
    Syslog(syslog::Error),
    /// Tried to initialize a file logger when another was already initialized
    FileDup(SetLoggerError),
}

impl Display for LoggingError {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        use LoggingError::*;
        match self {
            &LogLevel(e,s)  => {
                write!(fmt, "couldn't parse log level '{}': {}", e, s);
            },
            &LogFile(e,p)   => {
                write!(fmt, "couldn't open log file '{}': {}'", e, p);
            },
            &SystemdDup(e) => {
                write!(fmt, "couldn't initialize journald logging: {}", e);
            },
            &Syslog(e) => {
                write!(fmt, "couldn't initialize syslog logging: {}", e);
            }
            &FileDup(e)   => {
                write!(fmt, "can't initialize multiple loggers: {}", e);
            }
        }
    }
}

// #[cfg(feature = "std")] // I don't think I need this...?
impl std::error::Error for LoggingError {
    fn description(&self) -> &str {
        /*
         * match self {
         *     &LogLevel(_,s)  => format!("couldn't parse log level '{}'", s),
         *     &LogFile(_,p)   => format!("couldn't use log file '{}'", p),
         *     &SystemdDup(_) => "couldn't initialize journald logging",
         *     &Syslog(_)  => "couldn't initialize syslog logging",
         * }
         */
        "couldn't initialize logging"
    }

    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        use LoggingError::*;
        match self {
            &LogLevel(e,_)  => Some(e),
            &LogFile(e,_)   => Some(e),
            &SystemdDup(e) => Some(e),
            &Syslog(e)  => Some(e),
            &FileDup(e)   => Some(e),
        }
    }
}

/// Where we're logging to
#[derive(Debug, PartialEq)]
enum LogLocation {
    File(PathBuf),
    Systemd,
    Syslog,
}

impl Display for LogLocation {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        use LogLocation::*;
        match self {
            &File(p)    => write!(f, "{}", p),
            &Systemd    => write!(f, "systemd journal"),
            &Syslog => write!(f, "syslog"),
        }
    }
}

type LogInitResult = Result<LogLocation, LoggingError>;

/// Globally initialize the logger.
fn init_logger() -> LogInitResult {
    //FEEP: add filename parsing (e.g. `date`-style string)
    let logfile = CLI_ARGS.value_of("logfile").unwrap_or(DEFAULT_LOG_FILE);

    let loglvl = CLI_ARGS.value_of("loglvl")
        .map(|s|
             log::LevelFilter::from_str(s)
             .map_err(|e| LoggingError::LogLevel(e,s.to_owned()))?)
        .unwrap_or(DEFAULT_LOG_LEVEL);

    if "sysd" == logfile {
        init_sysd_journal(loglvl)
    } else if "system" == logfile {
        // We'd log the failure but there's nothing to log to!
        init_sysd_journal(loglvl).or_else(|e| {
            if ! is_quiet() {
                eprintln!("Couldn't init systemd journaling ({}); trying *nix syslog instead.", e);
            }
            init_splatnix_syslog(loglvl)
        })
    } else if "syslog" == logfile {
        init_splatnix_syslog(loglvl)
    } else {
        init_logfile(loglvl, logfile)
    }
}

/// Use the systemd journal as the logger
#[cfg(feature = "sysd")]
fn init_sysd_journal(level: LevelFilter) -> LogInitResult {
    // JournalLog::init()
    //     .map_err(|e| LoggingError::SystemdDup(e))
    //     .and_then(|_| log::set_max_level(loglvl); Ok(_))
    match systemd::JournalLog::init() {
        Ok(_)   => {
            log::set_max_level(level);
            Ok(LogLocation::Systemd)
        },
        Err(e)  => LoggingError::SystemdDup(e),
    }
}

/// Systemd support not compiled in - can't use it!
#[cfg(not(feature = "sysd"))]
fn init_sysd_journal(_level: LevelFilter) -> LogInitResult {
    LoggingError::NoSystemd
}

// as per
// https://rust-lang-nursery.github.io/rust-cookbook/development_tools/debugging/log.html#log-to-the-unix-syslog
/// Use the system log as the logger
fn init_splatnix_syslog(level: LevelFilter) -> LogInitResult {
    syslog::init(get_syslog_facility(),
    level, 
    Some("spinnrd"))
        .map_or_else(
            |e| LoggingError::Syslog(e),
            |_| LogLocation::Syslog)
}

/// Get the appropriate syslog facility
/// (DAEMON if daemonizing, USER otherwise)
fn get_syslog_facility() -> syslog::Facility {
    if is_daemon() {
        syslog::Facility::LOG_DAEMON
    } else {
        syslog::Facility::LOG_USER
    }
}

/// Initialize a file as the logger
fn init_logfile(loglvl: LevelFilter, logfile: &str) -> LogInitResult {
    let logpath = PathBuf::from(logfile);
    WriteLogger::init(loglvl,
                      simplelog::Config::default(),
                      open_log_file(&logpath)?)
        .map_or_else(
            |e| LoggingError::FileDup(e),
            |_| LogLocation::File(logpath))
}

/// Open the log file
fn open_log_file<W: Write + Send + 'static>(logfile: &PathBuf) -> Result<W,LoggingError> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(logfile)
        .map_err(|e| LoggingError::LogFile(e, logfile))
}

/// Log the failure to open a logfile
fn log_logging_failure<D: Display>(err: D) {
    // not using the filename formatting because it's unnecessary
    // and might cause its own problems.
    let logfailfile = LOG_FAIL_FILE.replace(
        "%d",
        chrono::Local::now()
        .format("%Y%m%dT%H%M%S%.f%z"));

    match File::create(logfailfile) {
        Ok(file)    => {
            write!(file, "{}", err)
                .unwrap_or_else(|e| eprintln!(
                        "Couldn't log logging error '{}' to {}. ({})",
                        err, logfailfile, e));
        },
        Err(e)  => {
            eprintln!("Couldn't open {} to log logging error '{}'. ({})", 
                      logfailfile, err, e);
        }
    }
}

/// Returns true if we are to daemonize
fn is_daemon() -> bool {
    //FIXME: writeme
}

/// Initialize the accelerometer
fn init_accel<A: Accelerometer>(mult: f64) -> Result<FilteredAccelerometer<A>,i32> {
    //FIXME: writeme
    Ok(FilteredAccelerometer::new(accel, mult));
}

/// Initializes the signal handler
fn init_sigtrap(sigs: &[Signal]) -> (thread::JoinHandle<()>, mpsc::Receiver<Signal>) {
    debug!("initializing signal trap...");
    let mut sigtrap = Trap::trap(sigs);
    let (tx, rx) = mpsc::sync_channel::<Signal>(1);
    let handle = thread::spawn(move || tx.send(sigtrap.next().unwrap()).unwrap());
    (handle, rx)
}


/// Something that can give the device's orientation.
pub trait Orientator {
    /// Returns the current orientation, if it can figure it out.
    fn orientation(&mut self) -> Option<Rotation>;
}

impl<T: Accelerometer> Orientator for FilteredAccelerometer<T> {
    fn orientation(&mut self) -> Option<Rotation> {
        let acc = self.read();
        if acc.z.abs() > 8.3385 { // 85% of g
            trace!("rot: {}; accel: {}", "None (z too high)", acc);
            None
        } else if (acc.x.abs() - acc.y.abs()).abs() > acc.z.abs() / 2.0 + 1.4715 {
            if acc.x.abs() > acc.y.abs() {
                if acc.x < 0.0 {
                    trace!("rot: {}; accel: {}", Rotation::Right, acc);
                    Some(Rotation::Right)
                } else {
                    trace!("rot: {}; accel: {}", Rotation::Left, acc);
                    Some(Rotation::Left)
                }
            } else {
                if acc.y < 0.0 {
                    trace!("rot: {}; accel: {}", Rotation::Normal, acc);
                    Some(Rotation::Normal)
                } else {
                    trace!("rot: {}; accel: {}", Rotation::Inverted, acc);
                    Some(Rotation::Inverted)
                }
            }
        } else {
            trace!("rot: {}; accel: {}", "None (dxy too low)", acc);
            None
        }
    }
}

#[derive(Debug,PartialEq,Clone,Copy)]
pub enum Rotation {
    Normal,
    Left,
    Inverted,
    Right,
}
use self::Rotation::*;

// pub struct RotParseErr (
#[derive(Debug)]
pub enum RotParseErrKind {
    TooShort,
    TooLong,
    NoMatch,
}

impl Default for Rotation {
    fn default() -> Rotation {
        Rotation::Normal
    }
}

impl Display for Rotation {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        match self {
            &Normal => write!(f, "normal"),
            &Left   => write!(f, "left"),
            &Inverted   => write!(f, "inverted"),
            &Right  => write!(f, "right"),
        }
    }
}

/// Returns true if we're not writing to stdout.
fn is_quiet() -> bool {
    //FIXME: writeme
}

/// Gets the path to the pid file.
fn get_pid_file() -> PathBuf {
    //FIXME: writeme
}

/// Gets the user spinnrd should run as
fn get_user() -> daemonize::User {
    //FIXME: writeme
}

/// Gets the group spinnrd should run as
fn get_group() -> daemonize::Group {
    //FIXME: writeme
}

/// Get the location of the spinfile
fn get_spinfile() -> PathBuf {
    //FIXME: writeme
}

/// Get the working directory (where files go by default)
fn get_working_dir() -> PathBuf {
    //FIXME: writeme
}

/// Get the u32 value of an argument to a command-line option.
/// Returns `None` if parsing fails.
fn get_u32_arg_val(name: &str) -> Option<u32> {
    if let Some(s) = CLI_ARGS.value_of(name) {
        s.parse::<u32>().map_err(|e|
                warn!("Can't parse '{}' as a uint ({})!", s, e)
            )
        .ok()
    } else { None }
}

/// Check that an argument is a valid u32
fn validate_u32(v: String) -> Result<(), String> {
    if "" == v { return Ok(()) };
    match v.parse::<u32>() {
        Ok(_)   => Ok(()),
        Err(e)  => Err(format!("Try using a positive integer, not {}. ({:?})",v,e)),
    }
}

/// Returns true if we should quit if an error occurs
/// when writing to the spinfile
fn quit_on_spinfile_write_error() -> bool {
    //FIXME: writeme
}

/// Returns true if we should quit if an error occurs
/// when opening the spinfile
fn quit_on_spinfile_open_error() -> bool {
    //FIXME: writeme
}

/// Remove the pid file at `p`.
fn rm_pid_file(p: &PathBuf) {
    match remove_file(p) {
        Ok(_)   => {},
        Err(e)  => {
            if ! is_quiet() {
                error!("Error removing PID file '{}': {}", p.display(), e);
            }
            // return 7i32;
        },
    }
}
