//! Side-effect-free typed tools used by the interactive verification mode.

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
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
    type ResolvedInput = LookupWeatherInput;
    type Output = LookupWeatherOutput;

    fn name(&self) -> ToolName {
        ToolName::new("lookup_weather").expect("fixed lookup_weather name is valid")
    }

    fn description(&self) -> String {
        "Look up weather for a city before answering weather questions. This is a deterministic \
         demo tool: its result is synthetic and must be described as demo data."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::ResolvedInput,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let city = input.city.trim();
        if city.is_empty() {
            return Err(ToolError::invalid_input("city must not be empty"));
        }
        Ok(ToolResolution::general(LookupWeatherInput {
            city: city.to_owned(),
        }))
    }

    fn execute<'a>(
        &'a self,
        input: Self::Input,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            Ok(LookupWeatherOutput {
                city: input.city,
                condition: "clear",
                temperature_c: 24,
                source: "runtime-harness deterministic demo",
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use agent_tools::{Tool, ToolRegistry};
    use serde_json::json;

    use super::*;

    #[test]
    fn schema_requires_city_and_rejects_unknown_fields() {
        let mut registry = ToolRegistry::new();
        registry
            .register(LookupWeatherTool)
            .expect("register weather tool");
        let snapshot = registry.snapshot();
        let definition = &snapshot.definitions()[0];
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
        let resolved = LookupWeatherTool
            .resolve(LookupWeatherInput {
                city: " Shanghai ".to_owned(),
            })
            .expect("weather resolve");
        let output = LookupWeatherTool
            .execute(resolved.into_input(), ToolContext::default())
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
            .resolve(LookupWeatherInput {
                city: " ".to_owned(),
            })
            .expect_err("blank city must fail");
        assert_eq!(error, ToolError::invalid_input("city must not be empty"));
    }
}
