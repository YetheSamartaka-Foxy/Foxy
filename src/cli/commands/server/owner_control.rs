use super::{CommandError, CommandSuccess, content_stage, instances};
use crate::cli::args::{CliArgs, ServerSuperviseArgs};
use crate::cli::exit_codes;
use crate::core::server::{ServerContext, owner};
use serde_json::json;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) fn supervise(
    cli: &CliArgs,
    args: ServerSuperviseArgs,
) -> Result<CommandSuccess, CommandError> {
    const ACTION: &str = "server.supervise";
    if args.background && !cfg!(windows) {
        return Err(CommandError::validation(
            ACTION,
            "--background is available only on Windows. Use foreground supervision for a Linux service.",
        ));
    }
    let context = instances::capture(args.context, ACTION)?;
    if args.detach || args.shutdown {
        if cli.dry_run {
            return Ok(CommandSuccess {
                action: ACTION.into(),
                message: "Owner lifetime preview; no owner was contacted or started.".into(),
                data: json!({"preview":true,"detach":args.detach,"shutdown":args.shutdown,"database_opened":false}),
                exit_code: exit_codes::SUCCESS,
            });
        }
        if !cli.yes {
            return Err(CommandError::validation(
                ACTION,
                "Changing owner lifetime requires --yes",
            ));
        }
        let snapshot = if args.detach {
            owner::start_detached(&context)
        } else {
            owner::attach(&context)
                .and_then(|attachment| {
                    attachment.ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            "No authenticated owner is available",
                        )
                    })
                })
                .and_then(|attachment| attachment.shutdown_owner())
        }
        .map_err(|error| owner_error(ACTION, error))?;
        if args.shutdown {
            wait_released(&context).map_err(|error| owner_error(ACTION, error))?;
        }
        return Ok(CommandSuccess {
            action: ACTION.into(),
            message: if args.detach {
                "Authenticated detached owner; closing this controller leaves its servers running."
            } else {
                "Idle owner shut down and released its game space."
            }
            .into(),
            data: json!({"owner":snapshot,"database_opened":false,"released":args.shutdown}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if cli.dry_run {
        return Err(CommandError::validation(
            ACTION,
            "Use --detach or --shutdown for an owner lifetime preview",
        ));
    }
    let stopped = Arc::new(AtomicBool::new(false));
    let _signal =
        if args.parent_bound {
            None
        } else {
            Some(content_stage::signal::SignalGuard::install().map_err(|()| {
                CommandError::operation(ACTION, "Cannot register foreground shutdown")
            })?)
        };
    let signal_stop = stopped.clone();
    let monitor = (!args.parent_bound).then(|| {
        std::thread::spawn(move || {
            while !signal_stop.load(Ordering::Acquire) {
                if content_stage::signal::requested() {
                    signal_stop.store(true, Ordering::Release);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    });
    let result =
        owner::supervise_with_launch(&context, args.parent_bound, args.launch_id, stopped.clone());
    stopped.store(true, Ordering::Release);
    if let Some(monitor) = monitor {
        let _ = monitor.join();
    }
    result.map_err(|error| owner_error(ACTION, error))?;
    Ok(CommandSuccess {
        action: ACTION.into(),
        message: "Owner drained and released its game space.".into(),
        data: json!({"database_opened":false,"released":true}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn owner_error(action: &str, error: io::Error) -> CommandError {
    CommandError {
        action: action.into(),
        message: format!("Owner lifetime operation failed: {error}"),
        code: if error.kind() == io::ErrorKind::WouldBlock {
            exit_codes::DATABASE_BUSY
        } else {
            exit_codes::OPERATION_FAILED
        },
    }
}

fn wait_released(context: &ServerContext) -> io::Result<()> {
    use crate::core::tasks::db_process_lock::{self, LockOutcome};
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match db_process_lock::acquire(&context.database_path()) {
            LockOutcome::Acquired => {
                return db_process_lock::release_for(&context.database_path())
                    .map_err(|_| io::Error::other("Owner release could not be verified"));
            }
            LockOutcome::Busy { .. } if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            LockOutcome::Busy { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The game space is still owned; shutdown was not verified",
                ));
            }
            LockOutcome::Unavailable(_) => {
                return Err(io::Error::other("Game-space release could not be verified"));
            }
        }
    }
}
