//! Side-effect-free typed tools used by the interactive verification mode.

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture};
use agent_types::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LookupWeatherInput {
    pub(crate) city: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct LookupWeatherOutput {
    pub(crate) city: String,
    pub(crate) condition: &'static str,
    pub(crate) temperature_c: i32,
    pub(crate) source: &'static str,
}

pub(crate) struct LookupWeatherTool;

impl Tool for LookupWeatherTool {
    type Input = LookupWeatherInput;
    type Output = LookupWeatherOutput;

    fn name(&self) -> ToolName {
        ToolName::new("lookup_weather").expect("fixed lookup_weather name is valid")
    }

    fn description(&self) -> &str {
        "Look up weather for a city before answering weather questions. This is a deterministic \
         demo tool: its result is synthetic and must be described as demo data."
    }

    fn execute<'a>(
        &'a self,
        input: Self::Input,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let city = input.city.trim();
            if city.is_empty() {
                return Err(ToolError::invalid_input("city must not be empty"));
            }
            Ok(LookupWeatherOutput {
                city: city.to_owned(),
                condition: "clear",
                temperature_c: 24,
                source: "runtime-harness deterministic demo",
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use agent_tools::Tool;
    use serde_json::json;

    use super::*;

    #[test]
    fn schema_requires_city_and_rejects_unknown_fields() {
        let definition = LookupWeatherTool.definition();
        assert_eq!(definition.name.as_str(), "lookup_weather");
        assert_eq!(definition.input_schema["required"], json!(["city"]));
        assert_eq!(
            definition.input_schema["additionalProperties"],
            json!(false)
        );
        assert!(
            serde_json::from_value::<LookupWeatherInput>(
                json!({"city": "Shanghai", "extra": true})
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn returns_fixed_demo_output_and_validates_non_empty_city() {
        let output = LookupWeatherTool
            .execute(
                LookupWeatherInput {
                    city: " Shanghai ".to_owned(),
                },
                ToolContext::default(),
            )
            .await
            .expect("weather lookup");
        assert_eq!(
            output,
            LookupWeatherOutput {
                city: "Shanghai".to_owned(),
                condition: "clear",
                temperature_c: 24,
                source: "runtime-harness deterministic demo",
            }
        );

        let error = LookupWeatherTool
            .execute(
                LookupWeatherInput {
                    city: " ".to_owned(),
                },
                ToolContext::default(),
            )
            .await
            .expect_err("blank city must fail");
        assert_eq!(error, ToolError::invalid_input("city must not be empty"));
    }
}
