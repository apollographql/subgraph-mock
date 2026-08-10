use crate::{
    error::{Error, Result},
    handle::graphql::ResponseGenerationConfig,
    latency::{LatencyConfig, LatencyGenerator},
};
use apollo_configuration::{ParseYamlOptions, configuration, expansion::EnvVariables};
use apollo_http_server_telemetry::HttpServerTelemetryConfig;
use apollo_opentelemetry::OpenTelemetryConfig;
use hyper::{
    HeaderMap,
    header::{HeaderName, HeaderValue},
};
use serde_json_bytes::serde_json;
use serde_yaml::Value;
use std::{collections::HashMap, fs, path::Path};
use tracing::{info, warn};

/// Allowed in the YAML, but not represented in the [BaseConfig] struct as we
/// neither want nor need that data structure to be recursive.
const SUBGRAPH_OVERRIDES_KEY: &str = "subgraph_overrides";

#[configuration]
struct BaseConfig {
    #[config(default = default_port())]
    pub port: u16,
    /// Seed for the server's random number generator. Omit for non-reproducible
    /// (OS-sourced) randomness. Global only: setting this in a subgraph override
    /// has no effect, since all subgraphs share a single RNG.
    pub seed: Option<u64>,
    pub headers: HashMap<String, String>,
    #[config(default = LatencyConfig::default_with_sine())]
    pub latency: LatencyConfig,
    pub response_generation: ResponseGenerationConfig,
    #[config(default = default_cache_responses())]
    pub cache_responses: bool,
    pub telemetry: TelemetrySection,
}

pub fn default_port() -> u16 {
    8080
}

fn default_cache_responses() -> bool {
    true
}

impl BaseConfig {
    pub fn into_parts(
        self,
    ) -> Result<(
        u16,
        bool,
        LatencyGenerator,
        HeaderMap<HeaderValue>,
        ResponseGenerationConfig,
        TelemetryConfig,
    )> {
        info!(config=%serde_json::to_string(&self.latency).unwrap(), "latency generation");
        let latency_generator = LatencyGenerator::new(self.latency);

        info!(headers=%serde_json::to_string(&self.headers).unwrap(), "additional headers");
        let additional_headers: Result<HeaderMap<HeaderValue>> = self
            .headers
            .into_iter()
            .map(|(k, v)| Ok((HeaderName::try_from(&k)?, HeaderValue::try_from(&v)?)))
            .collect();

        let mut response_generation = self.response_generation;
        response_generation.merge_default_scalars();

        info!(config=%serde_json::to_string(&response_generation).unwrap(), "response generation");

        Ok((
            self.port,
            self.cache_responses,
            latency_generator,
            additional_headers?,
            response_generation,
            self.telemetry.resolve(),
        ))
    }
}

#[configuration]
struct TelemetrySection {
    otel: Option<OpenTelemetryConfig>,
    http: Option<HttpServerTelemetryConfig>,
}

impl TelemetrySection {
    fn resolve(self) -> TelemetryConfig {
        TelemetryConfig {
            otel: self.otel.unwrap_or_else(disabled_open_telemetry),
            http: self.http.unwrap_or_else(default_http_telemetry),
        }
    }
}

fn disabled_open_telemetry() -> OpenTelemetryConfig {
    apollo_configuration::parse_yaml(
        "disabled: true\n",
        &ParseYamlOptions::default().variables(EnvVariables),
    )
    .expect("hand-written literal is valid YAML")
}

fn default_http_telemetry() -> HttpServerTelemetryConfig {
    apollo_configuration::parse_yaml(
        "spans:\n  request_body_size: true\n  response_body_size: true\n\
         metrics:\n  request_body_size: true\n  response_body_size: true\n",
        &ParseYamlOptions::default().variables(EnvVariables),
    )
    .expect("hand-written literal is valid YAML")
}

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub otel: OpenTelemetryConfig,
    pub http: HttpServerTelemetryConfig,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub headers: HeaderMap<HeaderValue>,
    pub latency_generator: LatencyGenerator,
    pub response_generation: ResponseGenerationConfig,
    pub cache_responses: bool,
    pub subgraph_overrides: SubgraphOverrides,
}

#[derive(Debug, Clone, Default)]
pub struct SubgraphOverrides {
    pub headers: HashMap<String, HeaderMap<HeaderValue>>,
    pub latency_generator: HashMap<String, LatencyGenerator>,
    pub response_generation: HashMap<String, ResponseGenerationConfig>,
    pub cache_responses: HashMap<String, bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            headers: Default::default(),
            latency_generator: LatencyGenerator::new(LatencyConfig::default_with_sine()),
            response_generation: Default::default(),
            cache_responses: default_cache_responses(),
            subgraph_overrides: Default::default(),
        }
    }
}

impl Config {
    /// Reads and parses a YAML config file into a resolved port, RNG seed, [Config], and
    /// [TelemetryConfig]
    pub fn from_file(path: &Path) -> Result<(u16, Option<u64>, Config, TelemetryConfig)> {
        let bytes = fs::read(path)?;
        let value = serde_yaml::from_slice(&bytes)?;

        Self::parse_yaml(value)
    }

    /// Parses a YAML file into a resolved port, RNG seed, [Config], and [TelemetryConfig]
    pub fn parse_yaml(mut base: Value) -> Result<(u16, Option<u64>, Config, TelemetryConfig)> {
        let mapping = base.as_mapping_mut().ok_or(Error::NotAMapping)?;

        let mut subgraph_cache_responses = HashMap::new();
        let mut subgraph_headers = HashMap::new();
        let mut subgraph_latency_generators = HashMap::new();
        let mut subgraph_response_generation_configs = HashMap::new();

        if let Some(overrides) = mapping.remove(SUBGRAPH_OVERRIDES_KEY) {
            match overrides {
                Value::Mapping(mapping) => {
                    for (subgraph_name, subgraph_override) in mapping {
                        let mut subgraph_config = base.clone();
                        let subgraph_name: String = serde_yaml::from_value(subgraph_name)?;

                        let override_mapping = subgraph_override.as_mapping().ok_or_else(|| {
                            Error::OverrideNotAMapping {
                                subgraph: subgraph_name.clone(),
                            }
                        })?;

                        if override_mapping.contains_key("port") {
                            warn!("port overrides for subgraphs will be ignored")
                        }

                        if override_mapping.contains_key("seed") {
                            warn!("seed overrides for subgraphs will be ignored")
                        }

                        if override_mapping.contains_key("telemetry") {
                            warn!("telemetry overrides for subgraphs will be ignored")
                        }

                        merge_yaml(subgraph_override, &mut subgraph_config);
                        let subgraph_config_text = serde_yaml::to_string(&subgraph_config)?;
                        let parsed_config: BaseConfig = apollo_configuration::parse_yaml(
                            &subgraph_config_text,
                            &ParseYamlOptions::default().variables(EnvVariables),
                        )?;

                        info!("generating customized config for {}", subgraph_name);
                        let (
                            _port,
                            cache_responses,
                            latency_generator,
                            headers,
                            response_generation,
                            _telemetry_config,
                        ) = parsed_config.into_parts()?;

                        subgraph_cache_responses.insert(subgraph_name.clone(), cache_responses);
                        subgraph_latency_generators
                            .insert(subgraph_name.clone(), latency_generator);
                        subgraph_headers.insert(subgraph_name.clone(), headers);
                        subgraph_response_generation_configs
                            .insert(subgraph_name, response_generation);
                    }
                }
                _ => return Err(Error::NotAMapping),
            }
        }

        let base_config_text = serde_yaml::to_string(&base)?;
        let base_config: BaseConfig = apollo_configuration::parse_yaml(
            &base_config_text,
            &ParseYamlOptions::default().variables(EnvVariables),
        )?;
        let seed = base_config.seed;
        info!(seed = ?seed, "rng seed");

        let (port, cache_responses, latency, headers, response_generation, telemetry_config) =
            base_config.into_parts()?;

        Ok((
            port,
            seed,
            Config {
                headers,
                latency_generator: latency,
                response_generation,
                cache_responses,
                subgraph_overrides: SubgraphOverrides {
                    headers: subgraph_headers,
                    latency_generator: subgraph_latency_generators,
                    response_generation: subgraph_response_generation_configs,
                    cache_responses: subgraph_cache_responses,
                },
            },
            telemetry_config,
        ))
    }
}

/// A function for merging yaml overrides with the base config.
/// It does *not* combine arrays, since arrays are effectively scalar values that should be replaced, not merged,
/// in the context of the subgraph config. We may also want to revisit the mapping merge logic if it ends up being
/// unintuitive in the context of configuration such as the latency waveforms.
fn merge_yaml(overrides: serde_yaml::Value, base: &mut serde_yaml::Value) {
    use serde_yaml::Value;

    match (overrides, base) {
        // If both values are mappings we add all keys from src into dst.
        (Value::Mapping(override_map), Value::Mapping(base_map)) => {
            for (key, override_val) in override_map.into_iter() {
                // If a key is present in both maps then we recursively merge the values,
                // otherwise we just insert the src key into dst directly.
                match base_map.get_mut(&key) {
                    Some(base_val) => merge_yaml(override_val, base_val),
                    None => _ = base_map.insert(key, override_val),
                };
            }
        }

        // Otherwise we replace base with overrides
        (overrides, base) => *base = overrides,
    }
}
