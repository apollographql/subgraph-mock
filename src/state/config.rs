use crate::{
    handle::graphql::ResponseGenerationConfig,
    latency::{LatencyConfig, LatencyGenerator},
};
use anyhow::Error;
use hyper::{
    HeaderMap,
    header::{HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json_bytes::serde_json;
use serde_yaml::Value;
use std::collections::HashMap;
use tracing::{info, warn};

/// Allowed in the YAML, but not represented in the [BaseConfig] struct as we
/// neither want nor need that data structure to be recursive.
const SUBGRAPH_OVERRIDES_KEY: &str = "subgraph_overrides";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BaseConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub latency: LatencyConfig,
    #[serde(default)]
    pub response_generation: ResponseGenerationConfig,
    #[serde(default = "default_cache_responses")]
    pub cache_responses: bool,
}

pub fn default_port() -> u16 {
    8080
}

fn default_cache_responses() -> bool {
    true
}

impl Default for BaseConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            headers: Default::default(),
            latency: Default::default(),
            response_generation: Default::default(),
            cache_responses: default_cache_responses(),
        }
    }
}

impl BaseConfig {
    pub fn into_parts(
        self,
    ) -> anyhow::Result<(
        u16,
        bool,
        LatencyGenerator,
        HeaderMap<HeaderValue>,
        ResponseGenerationConfig,
    )> {
        info!(config=%serde_json::to_string(&self.latency).unwrap(), "latency generation");
        let latency_generator = LatencyGenerator::new(self.latency);

        info!(headers=%serde_json::to_string(&self.headers).unwrap(), "additional headers");
        let additional_headers: anyhow::Result<HeaderMap<HeaderValue>> = self
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
        ))
    }
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
            latency_generator: LatencyGenerator::new(LatencyConfig::default()),
            response_generation: Default::default(),
            cache_responses: default_cache_responses(),
            subgraph_overrides: Default::default(),
        }
    }
}

impl Config {
    /// Parses a YAML file into a resolved port and [Config]
    pub fn parse_yaml(mut base: Value) -> anyhow::Result<(u16, Config)> {
        let mapping = base
            .as_mapping_mut()
            .ok_or_else(|| Error::msg("config file must be a mapping"))?;

        let mut subgraph_cache_responses = HashMap::new();
        let mut subgraph_headers = HashMap::new();
        let mut subgraph_latency_generators = HashMap::new();
        let mut subgraph_response_generation_configs = HashMap::new();

        if let Some(overrides) = mapping.remove(SUBGRAPH_OVERRIDES_KEY) {
            match overrides {
                Value::Mapping(mapping) => {
                    for (subgraph_name, subgraph_override) in mapping {
                        let mut subgraph_config = base.clone();

                        let override_mapping = subgraph_override
                            .as_mapping()
                            .ok_or_else(|| Error::msg("subgraph override must be a mapping"))?;

                        if override_mapping.contains_key("port") {
                            warn!("port overrides for subgraphs will be ignored")
                        }

                        merge_yaml(subgraph_override, &mut subgraph_config);
                        let parsed_config: BaseConfig = serde_yaml::from_value(subgraph_config)?;
                        let subgraph_name: String = serde_yaml::from_value(subgraph_name)?;

                        info!("generating customized config for {}", subgraph_name);
                        let (
                            _port,
                            cache_responses,
                            latency_generator,
                            headers,
                            response_generation,
                        ) = parsed_config.into_parts()?;

                        subgraph_cache_responses.insert(subgraph_name.clone(), cache_responses);
                        subgraph_latency_generators
                            .insert(subgraph_name.clone(), latency_generator);
                        subgraph_headers.insert(subgraph_name.clone(), headers);
                        subgraph_response_generation_configs
                            .insert(subgraph_name, response_generation);
                    }
                }
                _ => return Err(Error::msg("config file must be a mapping")),
            }
        }

        let (port, cache_responses, latency, headers, response_generation) =
            serde_yaml::from_value::<BaseConfig>(base)?.into_parts()?;

        Ok((
            port,
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
