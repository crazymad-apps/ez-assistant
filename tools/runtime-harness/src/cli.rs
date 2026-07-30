//! Command parsing for `list`, `verify`, and `chat`.

use std::{
    ffi::{OsStr, OsString},
    fmt,
};

use crate::HarnessError;

pub(crate) const HELP: &str = "\
runtime-harness — ez-assistant version verification host

USAGE:
  runtime-harness list
  runtime-harness verify v0.2|v0.3
  runtime-harness chat [--debug <url>] [--debug-layer provider|agent|both]
  runtime-harness --help

DEBUG:
  --debug <url>                 viewer URL; takes precedence over DEBUG_URL
  --debug-layer <selection>     provider, agent, or both (default: both)
";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Help,
    List,
    Verify {
        version: VersionBaseline,
    },
    Chat {
        debug_url: Option<String>,
        debug_layer: DebugLayerSelection,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VersionBaseline {
    V0_2,
    V0_3,
}

impl fmt::Display for VersionBaseline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V0_2 => formatter.write_str("v0.2"),
            Self::V0_3 => formatter.write_str("v0.3"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DebugLayerSelection {
    Provider,
    Agent,
    Both,
}

impl fmt::Display for DebugLayerSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider => formatter.write_str("provider"),
            Self::Agent => formatter.write_str("agent"),
            Self::Both => formatter.write_str("both"),
        }
    }
}

pub(crate) fn parse_env() -> Result<Command, HarnessError> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let debug_url = if args.first().is_some_and(|arg| arg == OsStr::new("chat")) {
        std::env::var_os("DEBUG_URL")
            .map(|value| {
                value.into_string().map_err(|_| {
                    HarnessError::Config("DEBUG_URL must contain valid UTF-8".to_owned())
                })
            })
            .transpose()?
    } else {
        None
    };
    parse_from(args, debug_url)
}

pub(crate) fn parse_from<I>(args: I, debug_url_env: Option<String>) -> Result<Command, HarnessError>
where
    I: IntoIterator<Item = OsString>,
{
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| HarnessError::Cli("arguments must contain valid UTF-8".to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some((command, rest)) = args.split_first() else {
        return Err(HarnessError::Cli(
            "missing command; run `runtime-harness --help`".to_owned(),
        ));
    };

    match command.as_str() {
        "--help" | "-h" | "help" => {
            require_no_args(command, rest)?;
            Ok(Command::Help)
        }
        "list" => {
            require_no_args(command, rest)?;
            Ok(Command::List)
        }
        "verify" => parse_verify(rest),
        "chat" => parse_chat(rest, debug_url_env),
        unknown => Err(HarnessError::Cli(format!(
            "unknown command `{unknown}`; run `runtime-harness --help`"
        ))),
    }
}

fn require_no_args(command: &str, args: &[String]) -> Result<(), HarnessError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(HarnessError::Cli(format!(
            "`{command}` does not accept arguments"
        )))
    }
}

fn parse_verify(args: &[String]) -> Result<Command, HarnessError> {
    match args {
        [version] if version == "v0.2" || version == "v0.3" => Ok(Command::Verify {
            version: if version == "v0.2" {
                VersionBaseline::V0_2
            } else {
                VersionBaseline::V0_3
            },
        }),
        [] => Err(HarnessError::Cli(
            "`verify` requires a version; supported: v0.2, v0.3".to_owned(),
        )),
        [version] => Err(HarnessError::Cli(format!(
            "unsupported version `{version}`; supported: v0.2, v0.3"
        ))),
        _ => Err(HarnessError::Cli(
            "`verify` accepts exactly one version".to_owned(),
        )),
    }
}

fn parse_chat(args: &[String], debug_url_env: Option<String>) -> Result<Command, HarnessError> {
    let mut debug_url = None;
    let mut debug_layer = None;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if argument == "--debug" {
            reject_duplicate("--debug", debug_url.is_some())?;
            index += 1;
            let value = args.get(index).ok_or_else(|| {
                HarnessError::Cli("`--debug` requires a non-empty URL".to_owned())
            })?;
            debug_url = Some(non_empty_value("--debug", value)?);
        } else if let Some(value) = argument.strip_prefix("--debug=") {
            reject_duplicate("--debug", debug_url.is_some())?;
            debug_url = Some(non_empty_value("--debug", value)?);
        } else if argument == "--debug-layer" {
            reject_duplicate("--debug-layer", debug_layer.is_some())?;
            index += 1;
            let value = args.get(index).ok_or_else(|| {
                HarnessError::Cli("`--debug-layer` requires provider, agent, or both".to_owned())
            })?;
            debug_layer = Some(parse_debug_layer(value)?);
        } else if let Some(value) = argument.strip_prefix("--debug-layer=") {
            reject_duplicate("--debug-layer", debug_layer.is_some())?;
            debug_layer = Some(parse_debug_layer(value)?);
        } else {
            return Err(HarnessError::Cli(format!(
                "unknown chat argument `{argument}`"
            )));
        }
        index += 1;
    }

    let debug_url = debug_url.or_else(|| debug_url_env.filter(|value| !value.trim().is_empty()));
    Ok(Command::Chat {
        debug_url,
        debug_layer: debug_layer.unwrap_or(DebugLayerSelection::Both),
    })
}

fn reject_duplicate(argument: &str, is_duplicate: bool) -> Result<(), HarnessError> {
    if is_duplicate {
        Err(HarnessError::Cli(format!(
            "`{argument}` may only be provided once"
        )))
    } else {
        Ok(())
    }
}

fn non_empty_value(argument: &str, value: &str) -> Result<String, HarnessError> {
    if value.trim().is_empty() {
        Err(HarnessError::Cli(format!(
            "`{argument}` requires a non-empty value"
        )))
    } else {
        Ok(value.to_owned())
    }
}

fn parse_debug_layer(value: &str) -> Result<DebugLayerSelection, HarnessError> {
    match value {
        "provider" => Ok(DebugLayerSelection::Provider),
        "agent" => Ok(DebugLayerSelection::Agent),
        "both" => Ok(DebugLayerSelection::Both),
        _ => Err(HarnessError::Cli(format!(
            "invalid debug layer `{value}`; expected provider, agent, or both"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn parse_ok(values: &[&str], debug_url_env: Option<String>) -> Command {
        parse_from(args(values), debug_url_env).expect("command must parse")
    }

    #[test]
    fn parses_supported_commands_and_defaults() {
        assert_eq!(parse_ok(&["--help"], None), Command::Help);
        assert_eq!(parse_ok(&["list"], None), Command::List);
        assert_eq!(
            parse_ok(&["verify", "v0.2"], None),
            Command::Verify {
                version: VersionBaseline::V0_2,
            }
        );
        assert_eq!(
            parse_ok(&["verify", "v0.3"], None),
            Command::Verify {
                version: VersionBaseline::V0_3,
            }
        );
        assert_eq!(
            parse_ok(&["chat"], None),
            Command::Chat {
                debug_url: None,
                debug_layer: DebugLayerSelection::Both,
            }
        );
    }

    #[test]
    fn cli_debug_url_overrides_environment_and_options_accept_both_forms() {
        assert_eq!(
            parse_ok(
                &[
                    "chat",
                    "--debug-layer",
                    "agent",
                    "--debug=http://localhost:7331",
                ],
                Some("http://environment:7331".to_owned()),
            ),
            Command::Chat {
                debug_url: Some("http://localhost:7331".to_owned()),
                debug_layer: DebugLayerSelection::Agent,
            }
        );
        assert_eq!(
            parse_ok(
                &[
                    "chat",
                    "--debug",
                    "http://localhost:7332",
                    "--debug-layer=provider",
                ],
                None,
            ),
            Command::Chat {
                debug_url: Some("http://localhost:7332".to_owned()),
                debug_layer: DebugLayerSelection::Provider,
            }
        );
    }

    #[test]
    fn environment_debug_url_is_only_a_chat_fallback() {
        assert_eq!(
            parse_ok(&["chat"], Some("http://environment:7331".to_owned())),
            Command::Chat {
                debug_url: Some("http://environment:7331".to_owned()),
                debug_layer: DebugLayerSelection::Both,
            }
        );
        assert_eq!(
            parse_ok(&["list"], Some("ignored".to_owned())),
            Command::List
        );
    }

    #[test]
    fn rejects_missing_unknown_duplicate_and_invalid_arguments() {
        for invalid in [
            vec![],
            args(&["unknown"]),
            args(&["verify"]),
            args(&["verify", "v0.1"]),
            args(&["verify", "v0.2", "extra"]),
            args(&["list", "extra"]),
            args(&["chat", "--debug"]),
            args(&["chat", "--debug="]),
            args(&["chat", "--debug-layer", "other"]),
            args(&["chat", "--debug", "one", "--debug", "two"]),
            args(&["chat", "--debug-layer=agent", "--debug-layer=both"]),
        ] {
            assert!(parse_from(invalid, None).is_err());
        }
    }
}
