mod output;

use clap::{Parser, Subcommand};
use output::Output;
use std::{path::PathBuf, process::ExitCode};
use synapsed_api::{ApiError, Characteristic, CharacteristicCommand, ErrorCode};
use synapsed_client::{Client, ClientError, ClientEvent, config, parse_value, reconnect_delay};

#[derive(Parser)]
#[command(
    name = "synapsectl",
    version,
    about = "Inspect and control the local Synapsed characteristic service"
)]
struct Arguments {
    #[arg(
        long,
        global = true,
        help = "HTTP loopback origin; overrides environment and client configuration"
    )]
    endpoint: Option<String>,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Explicit public client.toml configuration"
    )]
    client_config: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Exact API JSON; follow commands emit NDJSON"
    )]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Status,
    Inventory,
    Devices,
    Get {
        device: String,
        characteristic: String,
    },
    Set {
        device: String,
        characteristic: String,
        #[arg(
            allow_hyphen_values = true,
            help = "Complete number, exact true/false, enum value, or literal string (including quoted empty string)"
        )]
        value: String,
    },
    Step {
        device: String,
        characteristic: String,
        #[arg(allow_hyphen_values = true, value_parser = clap::value_parser!(i32).range(-100..=100))]
        steps: i32,
    },
    Toggle {
        device: String,
        characteristic: String,
    },
    Invoke {
        device: String,
        characteristic: String,
    },
    Watch,
    Logs {
        #[arg(long)]
        follow: bool,
    },
}

#[derive(Debug)]
enum Failure {
    Client(ClientError),
    Output,
}
impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        Self::Client(error)
    }
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Output
    }
}
impl Failure {
    fn code(&self) -> u8 {
        match self {
            Self::Client(ClientError::Config(_) | ClientError::InvalidCommand(_)) => 2,
            _ => 1,
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Client(error) => error.fmt(formatter),
            Self::Output => formatter.write_str("Cannot write command output."),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let endpoint = match config::load_endpoint(
        arguments.endpoint.as_deref(),
        arguments.client_config.as_deref(),
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("synapsectl: {error}");
            return ExitCode::from(2);
        }
    };
    let client = match Client::new(endpoint) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("synapsectl: {error}");
            return ExitCode::from(2);
        }
    };
    let output = Output {
        json: arguments.json,
    };
    let result = tokio::select! {
        result = run(&client, &output, arguments.command) => {
            result
        }
        signal = tokio::signal::ctrl_c() => {
            match signal {
                Ok(()) => Ok(()),
                Err(_) => Err(Failure::Client(ClientError::Network)),
            }
        }
    };
    client.close();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("synapsectl: {error}");
            ExitCode::from(error.code())
        }
    }
}

async fn run(client: &Client, output: &Output, command: Command) -> Result<(), Failure> {
    match command {
        Command::Status => output.status(&client.status().await?)?,
        Command::Inventory => output.inventory(&client.inventory().await?)?,
        Command::Devices => output.snapshot(client.snapshot().await?.as_ref())?,
        Command::Get {
            device,
            characteristic,
        } => {
            let metadata = get_characteristic(client, &device, &characteristic).await?;
            output.characteristic(&device, &metadata)?;
        }
        Command::Set {
            device,
            characteristic,
            value,
        } => {
            let metadata = get_characteristic(client, &device, &characteristic).await?;
            let value = parse_value(&metadata, &value)?;
            mutate(
                client,
                output,
                &device,
                &characteristic,
                &metadata,
                CharacteristicCommand::Set { value },
            )
            .await?;
        }
        Command::Step {
            device,
            characteristic,
            steps,
        } => {
            let metadata = get_characteristic(client, &device, &characteristic).await?;
            mutate(
                client,
                output,
                &device,
                &characteristic,
                &metadata,
                CharacteristicCommand::Step { steps },
            )
            .await?;
        }
        Command::Toggle {
            device,
            characteristic,
        } => {
            let metadata = get_characteristic(client, &device, &characteristic).await?;
            mutate(
                client,
                output,
                &device,
                &characteristic,
                &metadata,
                CharacteristicCommand::Toggle {},
            )
            .await?;
        }
        Command::Invoke {
            device,
            characteristic,
        } => {
            let metadata = get_characteristic(client, &device, &characteristic).await?;
            mutate(
                client,
                output,
                &device,
                &characteristic,
                &metadata,
                CharacteristicCommand::Invoke {},
            )
            .await?;
        }
        Command::Watch => follow(client, output, false).await?,
        Command::Logs { follow: true } => follow(client, output, true).await?,
        Command::Logs { follow: false } => output.logs(&client.logs().await?)?,
    }
    Ok(())
}
async fn get_characteristic(
    client: &Client,
    device: &str,
    characteristic: &str,
) -> Result<Characteristic, ClientError> {
    client
        .device(device)
        .await?
        .device
        .characteristics
        .into_iter()
        .find(|field| field.id == characteristic)
        .ok_or_else(|| {
            ClientError::Api(ApiError::new(
                ErrorCode::UnknownCharacteristic,
                "Unknown characteristic.",
            ))
        })
}
async fn mutate(
    client: &Client,
    output: &Output,
    device: &str,
    characteristic: &str,
    metadata: &Characteristic,
    command: CharacteristicCommand,
) -> Result<(), Failure> {
    metadata
        .validate_command(&command)
        .map_err(ClientError::Api)?;
    let snapshot = client.execute(device, characteristic, command).await?;
    output.snapshot(snapshot.as_ref())?;
    Ok(())
}

async fn follow(client: &Client, output: &Output, logs: bool) -> Result<(), Failure> {
    let mut attempt = 0;
    loop {
        eprintln!("synapsectl: connecting to local Synapsed service");
        let error = match client.events().await {
            Ok(mut events) => {
                attempt = 0;
                eprintln!(
                    "synapsectl: connected (instance {})",
                    events.initial_snapshot().node.instance_id
                );
                if !logs {
                    output.snapshot(events.initial_snapshot().as_ref())?;
                }
                loop {
                    match events.next().await {
                        Ok(Some(ClientEvent::Snapshot(snapshot))) if !logs => {
                            output.snapshot(snapshot.as_ref())?
                        }
                        Ok(Some(ClientEvent::Logs(entries))) if logs => {
                            for entry in entries {
                                output.log(&entry)?;
                            }
                        }
                        Ok(Some(ClientEvent::Log(entry))) if logs => output.log(&entry)?,
                        Ok(Some(_)) => {}
                        Ok(None) => break ClientError::Network,
                        Err(error) => break error,
                    }
                }
            }
            Err(error) => error,
        };
        if error.is_terminal() || matches!(error, ClientError::Closed) {
            return Err(error.into());
        }
        let delay = reconnect_delay(attempt);
        attempt = attempt.saturating_add(1);
        eprintln!(
            "synapsectl: disconnected; last observations are stale; {error} Reconnecting in {} seconds.",
            delay.as_secs()
        );
        tokio::time::sleep(delay).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_steps_zero_and_bounds_are_usage_semantics() {
        for steps in ["-100", "-1", "0", "100"] {
            let arguments =
                Arguments::try_parse_from(["synapsectl", "step", "desk", "height", steps]).unwrap();
            assert!(
                matches!(arguments.command, Command::Step { steps: value, .. } if value.to_string() == steps)
            );
        }
        for steps in ["-101", "101", "1.5", "12garbage"] {
            let error = Arguments::try_parse_from(["synapsectl", "step", "desk", "height", steps])
                .err()
                .unwrap();
            assert_eq!(error.exit_code(), 2);
        }
    }
    #[test]
    fn empty_strings_and_negative_numeric_set_are_preserved() {
        for value in ["", "-1.25", "two words"] {
            let arguments =
                Arguments::try_parse_from(["synapsectl", "set", "desk", "value", value]).unwrap();
            assert!(
                matches!(arguments.command, Command::Set { value: actual, .. } if actual == value)
            );
        }
    }
    #[test]
    fn inventory_command_parses_without_arguments() {
        let arguments = Arguments::try_parse_from(["synapsectl", "inventory"]).unwrap();
        assert!(matches!(arguments.command, Command::Inventory));
    }
}
