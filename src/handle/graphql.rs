use crate::{
    ADDITIONAL_HEADERS, CACHE_RESPONSES, RESPONSE_GENERATION_CONFIG, SUBGRAPH_CACHE_RESPONSES,
    SUBGRAPH_HEADERS, SUBGRAPH_RESPONSE_GENERATION_CONFIGS, SUPERGRAPH_SCHEMA,
    handle::ByteResponse,
};
use anyhow::anyhow;
use apollo_compiler::{
    ExecutableDocument, Name, Node, Schema,
    ast::OperationType,
    executable::{Field, Selection, SelectionSet},
    schema::ExtendedType,
    validation::{Valid, WithErrors},
};
use cached::proc_macro::cached;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{
    HeaderMap, Response, StatusCode,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};
use rand::{Rng, rngs::ThreadRng, seq::IteratorRandom};
use serde::{Deserialize, Serialize};
use serde_json_bytes::{
    ByteString, Map, Value, json,
    serde_json::{self, Number},
};
use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    mem,
    ops::RangeInclusive,
    sync::atomic::Ordering,
};
use tracing::{debug, error, trace};

pub async fn handle(
    body_bytes: Vec<u8>,
    subgraph_name: Option<&str>,
) -> anyhow::Result<ByteResponse> {
    let req: GraphQLRequest = match serde_json::from_slice(&body_bytes) {
        Ok(req) => req,
        Err(err) => {
            error!(%err, "received invalid graphql request");
            let mut resp = Response::new(
                Full::new(err.to_string().into_bytes().into())
                    .map_err(|never| match never {})
                    .boxed(),
            );
            *resp.status_mut() = StatusCode::BAD_REQUEST;

            return Ok(resp);
        }
    };

    let mut hasher = DefaultHasher::new();
    req.query.hash(&mut hasher);

    let cfg = subgraph_name
        .and_then(|name| {
            SUBGRAPH_RESPONSE_GENERATION_CONFIGS
                .wait()
                .get(name)
                // if this subgraph has an overridden response generation config, add the subgraph name to the hash
                // so a response that conforms to the subgraph's config is added to the cache rather than re-using
                // the standard cached response for this query
                .inspect(|_| name.hash(&mut hasher))
        })
        .unwrap_or_else(|| RESPONSE_GENERATION_CONFIG.wait());

    let query_hash = hasher.finish();

    if let Some((numerator, denominator)) = cfg.http_error_ratio {
        let mut rng = rand::rng();
        if rng.random_ratio(numerator, denominator) {
            return Response::builder()
                .status(rng.random_range(500..=504))
                .body(Empty::new().map_err(|never| match never {}).boxed())
                .map_err(|err| err.into());
        }
    }

    let (bytes, status_code) = if subgraph_name
        .and_then(|name| SUBGRAPH_CACHE_RESPONSES.wait().get(name).copied())
        .unwrap_or_else(|| CACHE_RESPONSES.load(Ordering::Relaxed))
    {
        into_response_bytes_and_status_code(cfg, req, query_hash).await
    } else {
        into_response_bytes_and_status_code_no_cache(cfg, req, query_hash).await
    };

    let mut resp = Response::new(Full::new(bytes).map_err(|never| match never {}).boxed());
    *resp.status_mut() = status_code;

    let headers = resp.headers_mut();
    add_headers(cfg, subgraph_name, headers);

    Ok(resp)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLRequest {
    pub query: String,
    pub operation_name: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, Value>,
    // #[serde(default)]
    // extensions: serde_json::Map<String, Value>,
}

fn add_headers(
    cfg: &ResponseGenerationConfig,
    subgraph_name: Option<&str>,
    headers: &mut HeaderMap,
) {
    let mut rng = rand::rng();

    // HeaderMap is a multimap and yields Some(HeaderName) only for the first element of each multimap.
    // We have to track the last one we saw and treat that as the key for all subsequent None values as such.
    // Based on that contract, the first iteration will *always* yield a value so we can safely just initialize
    // this to a dummy value and trust that it will get overwritten instead of using an Option.
    let mut last_header_name: HeaderName = HeaderName::from_static("unused");
    let mut last_ratio: Option<Ratio> = None;

    for (header_name, header_value) in subgraph_name
        .and_then(|name| SUBGRAPH_HEADERS.wait().get(name).cloned())
        .unwrap_or_else(|| ADDITIONAL_HEADERS.wait().clone())
        .into_iter()
    {
        if let Some(name) = header_name {
            last_ratio = cfg.header_ratio.get(name.as_str()).copied();
            last_header_name = name;
        }

        let should_insert = last_ratio
            .is_none_or(|(numerator, denominator)| rng.random_ratio(numerator, denominator));

        if should_insert {
            headers.insert(&last_header_name, header_value);
        }
    }

    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
}

#[cached(result = true, key = "u64", convert = "{_query_hash}")]
fn parse_and_validate(
    req: &GraphQLRequest,
    schema: &Valid<Schema>,
    _query_hash: u64,
) -> Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>> {
    let op_name = req.operation_name.as_deref().unwrap_or("unknown");

    ExecutableDocument::parse_and_validate(schema, &req.query, op_name)
}

#[tracing::instrument(skip(req))]
#[cached(key = "u64", convert = "{query_hash}")]
async fn into_response_bytes_and_status_code(
    cfg: &ResponseGenerationConfig,
    req: GraphQLRequest,
    query_hash: u64,
) -> (Bytes, StatusCode) {
    let schema = SUPERGRAPH_SCHEMA.wait();

    debug!(%query_hash, "handling graphql request");
    trace!(variables=?req.variables, "request variables");

    let doc = match parse_and_validate(&req, schema, query_hash) {
        Ok(doc) => doc,
        Err(err) => {
            let errs: Vec<_> = err.errors.iter().map(|d| d.to_json()).collect();
            error!(?errs, query=%req.query, "invalid graphql query");
            let bytes = serde_json::to_vec(&json!({ "data": Value::Null, "errors": errs }))
                .unwrap_or_default();
            return (bytes.into(), StatusCode::BAD_REQUEST);
        }
    };

    let op = doc.operations.iter().next().unwrap();
    let op_name = op.name.as_ref().map(|name| name.as_str());

    debug!(
        ?op_name,
        type=%op.operation_type,
        n_selections = op.selection_set.selections.len(),
        "processing operation"
    );

    let resp = match op.operation_type {
        OperationType::Query => match generate_response(cfg, op_name, &doc, schema) {
            Ok(resp) => resp,
            Err(err) => {
                error!(%err, "unable to generate response");
                return (
                    Bytes::from("unable to generate response"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        },

        // Not currently supporting mutations or subscriptions
        op_type => {
            error!("received {op_type} request: not implemented");
            return (
                Bytes::from("not implemented"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    match serde_json::to_vec(&resp) {
        Ok(bytes) => (bytes.into(), StatusCode::OK),
        Err(err) => {
            error!(%err, "unable to serialize response");
            (
                Bytes::from(err.to_string().into_bytes()),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

fn generate_response(
    cfg: &ResponseGenerationConfig,
    op_name: Option<&str>,
    doc: &Valid<ExecutableDocument>,
    schema: &Valid<Schema>,
) -> anyhow::Result<Value> {
    let op = match doc.operations.get(op_name) {
        Ok(op) => op,
        Err(_) => return Ok(json!({ "data": null })),
    };
    let mut rng = rand::rng();

    if let Some((numerator, denominator)) = cfg.graphql_errors.request_error_ratio
        && rng.random_ratio(numerator, denominator)
    {
        return Ok(json!({ "data": null, "errors": [{ "message": "Request error simulated" }]}));
    }

    let mut data =
        ResponseBuilder::new(&mut rng, doc, schema, cfg).selection_set(&op.selection_set)?;

    // Select a random number of top-level fields to "fail" if we are going to have field errors. For the sake of
    // simplicity and performance, we won't traverse deeper into the response object.
    if let Some((numerator, denominator)) = cfg.graphql_errors.field_error_ratio
        && rng.random_ratio(numerator, denominator)
    {
        let drop_count = rng.random_range(1..=data.len());
        let sampled_keys = data.keys().cloned().choose_multiple(&mut rng, drop_count);
        let to_drop: HashSet<ByteString> = HashSet::from_iter(sampled_keys);

        data.retain(|key, _| !to_drop.contains(key));

        let errors: Vec<Value> = to_drop
            .into_iter()
            .map(|key| {
                json!({
                    "message": "Field error simulated",
                    "path": [key]
                })
            })
            .collect();

        Ok(json!({
            "data": data,
            "errors": errors,
        }))
    } else {
        Ok(json!({ "data": data }))
    }
}

pub type Ratio = (u32, u32);

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GraphQLErrorConfig {
    /// The ratio of GraphQL requests that should be responded to with a request error and no data.
    ///
    /// Defaults to no requests containing errors.
    pub request_error_ratio: Option<Ratio>,
    /// The ratio of GraphQL requests that should include field-level errors and partial data.
    /// Note that if both this field and the request error ratio are set, this ratio will be applicable
    /// to the subset of requests that do not have request errors.
    ///
    /// For example, if you have a `request_error_ratio` of `[1,3]`, and a `field_error_ratio` of `[1,4]`,
    /// then only 1 in 6 of your total requests will contain field errors.
    ///
    /// Defaults to no requests containing errors.
    pub field_error_ratio: Option<Ratio>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseGenerationConfig {
    #[serde(default = "default_scalar_config")]
    pub scalars: HashMap<String, ScalarGenerator>,
    #[serde(default = "default_array_size")]
    pub array: ArraySize,
    #[serde(default = "default_null_ratio")]
    pub null_ratio: Option<Ratio>,
    #[serde(default)]
    pub header_ratio: HashMap<String, Ratio>,
    #[serde(default)]
    pub http_error_ratio: Option<Ratio>,
    #[serde(default)]
    pub graphql_errors: GraphQLErrorConfig,
}

impl ResponseGenerationConfig {
    /// Merges the default scalar config with the provided config, allowing users to specify a partial set of scalar
    /// generators while inheriting the default configuration for those they do not specify.
    pub fn merge_default_scalars(&mut self) {
        let default = default_scalar_config();
        let provided = mem::replace(&mut self.scalars, default);
        self.scalars.extend(provided);
    }
}

impl Default for ResponseGenerationConfig {
    fn default() -> Self {
        Self {
            scalars: default_scalar_config(),
            array: default_array_size(),
            null_ratio: default_null_ratio(),
            header_ratio: HashMap::new(),
            graphql_errors: GraphQLErrorConfig::default(),
            http_error_ratio: None,
        }
    }
}

fn default_scalar_config() -> HashMap<String, ScalarGenerator> {
    [
        ("Boolean".into(), ScalarGenerator::Bool),
        ("Int".into(), ScalarGenerator::Int { min: 0, max: 100 }),
        ("ID".into(), ScalarGenerator::Int { min: 0, max: 100 }),
        (
            "Float".into(),
            ScalarGenerator::Float {
                min: -1.0,
                max: 1.0,
            },
        ),
        (
            "String".into(),
            ScalarGenerator::String {
                min_len: 1,
                max_len: 10,
            },
        ),
    ]
    .into_iter()
    .collect()
}

fn default_array_size() -> ArraySize {
    ArraySize {
        min_length: 0,
        max_length: 10,
    }
}

fn default_null_ratio() -> Option<Ratio> {
    Some((1, 2))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ScalarGenerator {
    Bool,
    Float { min: f64, max: f64 },
    Int { min: i32, max: i32 },
    String { min_len: usize, max_len: usize },
}

impl Default for ScalarGenerator {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl ScalarGenerator {
    const DEFAULT: Self = Self::String {
        min_len: 1,
        max_len: 10,
    };

    fn generate(&self, rng: &mut ThreadRng) -> anyhow::Result<Value> {
        let val = match *self {
            Self::Bool => Value::Bool(rng.random_bool(0.5)),
            Self::Int { min, max } => Value::Number(rng.random_range(min..=max).into()),

            Self::Float { min, max } => Value::Number(
                Number::from_f64(rng.random_range(min..=max)).expect("expected finite float"),
            ),

            // The default Arbitrary impl for String has a random length so we build based on
            // characters instead
            Self::String { min_len, max_len } => {
                let len = rng.random_range(min_len..=max_len);
                // Allow for some multibyte chars. May still need to realloc
                let mut chars = Vec::with_capacity(len * 2);
                for _ in 0..len {
                    chars.push(rng.random::<char>());
                }

                Value::String(ByteString::from(chars.into_iter().collect::<String>()))
            }
        };

        Ok(val)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ArraySize {
    pub min_length: usize,
    pub max_length: usize,
}

impl ArraySize {
    fn range(&self) -> RangeInclusive<usize> {
        self.min_length..=self.max_length
    }
}

struct ResponseBuilder<'a, 'doc, 'schema> {
    rng: &'a mut ThreadRng,
    doc: &'doc Valid<ExecutableDocument>,
    schema: &'schema Valid<Schema>,
    cfg: &'a ResponseGenerationConfig,
}

impl<'a, 'doc, 'schema> ResponseBuilder<'a, 'doc, 'schema> {
    fn new(
        rng: &'a mut ThreadRng,
        doc: &'doc Valid<ExecutableDocument>,
        schema: &'schema Valid<Schema>,
        cfg: &'a ResponseGenerationConfig,
    ) -> Self {
        Self {
            rng,
            doc,
            schema,
            cfg,
        }
    }

    fn selection_set(
        &mut self,
        selection_set: &SelectionSet,
    ) -> anyhow::Result<Map<ByteString, Value>> {
        let grouped_fields = self.collect_fields(selection_set)?;
        let mut result = Map::new();

        for (key, fields) in grouped_fields {
            // The first occurrence of a field is representative for metadata that is defined by the schema
            let meta_field = fields[0];

            let val = if meta_field.name == "__typename" {
                Value::String(ByteString::from(selection_set.ty.to_string()))
            } else if !meta_field.ty().is_non_null() && self.should_be_null() {
                Value::Null
            } else {
                let is_selection_set = !meta_field.selection_set.is_empty();
                let is_array = meta_field.ty().is_list();

                if is_selection_set {
                    let mut selections = Vec::new();
                    for field in fields {
                        selections.extend_from_slice(&field.selection_set.selections);
                    }
                    let full_selection_set = SelectionSet {
                        ty: meta_field.selection_set.ty.clone(),
                        selections,
                    };

                    if is_array {
                        Value::Array(self.array_selection_set(&full_selection_set)?)
                    } else {
                        Value::Object(self.selection_set(&full_selection_set)?)
                    }
                } else {
                    match is_array {
                        false => self.leaf_field(meta_field.ty().inner_named_type())?,
                        true => self.array_leaf_field(meta_field.ty().inner_named_type())?,
                    }
                }
            };

            result.insert(key, val);
        }

        Ok(result)
    }

    fn collect_fields(
        &self,
        selection_set: &'doc SelectionSet,
    ) -> anyhow::Result<HashMap<String, Vec<&'doc Node<Field>>>> {
        let mut collected_fields: HashMap<String, Vec<&Node<Field>>> = HashMap::new();

        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    let key = field.alias.as_ref().unwrap_or(&field.name).to_string();
                    collected_fields.entry(key).or_default().push(field);
                }
                Selection::FragmentSpread(fragment) => {
                    if let Some(fragment_def) = self.doc.fragments.get(&fragment.fragment_name) {
                        for (key, mut fields) in self.collect_fields(&fragment_def.selection_set)? {
                            collected_fields.entry(key).or_default().append(&mut fields);
                        }
                    }
                }
                Selection::InlineFragment(inline_fragment) => {
                    for (key, mut fields) in self.collect_fields(&inline_fragment.selection_set)? {
                        collected_fields.entry(key).or_default().append(&mut fields);
                    }
                }
            }
        }

        Ok(collected_fields)
    }

    fn leaf_field(&mut self, type_name: &Name) -> anyhow::Result<Value> {
        match self.schema.types.get(type_name).unwrap() {
            ExtendedType::Enum(enum_ty) => {
                let enum_value = enum_ty
                    .values
                    .values()
                    .choose(self.rng)
                    .ok_or(anyhow!("empty enum: {type_name}"))?;

                Ok(Value::String(ByteString::from(
                    enum_value.value.to_string(),
                )))
            }

            ExtendedType::Scalar(scalar) => self
                .cfg
                .scalars
                .get(scalar.name.as_str())
                .unwrap_or(&ScalarGenerator::DEFAULT)
                .generate(self.rng),

            _ => unreachable!("A field with an empty selection set must be a scalar or enum type"),
        }
    }

    fn arbitrary_array_len(&mut self) -> anyhow::Result<usize> {
        Ok(self.rng.random_range(self.cfg.array.range()))
    }

    fn array_selection_set(&mut self, selection_set: &SelectionSet) -> anyhow::Result<Vec<Value>> {
        let num_values = self.arbitrary_array_len()?;
        let mut values = Vec::with_capacity(num_values);
        for _ in 0..num_values {
            values.push(Value::Object(self.selection_set(selection_set)?));
        }

        Ok(values)
    }

    fn array_leaf_field(&mut self, type_name: &Name) -> anyhow::Result<Value> {
        let num_values = self.arbitrary_array_len()?;
        let mut values = Vec::with_capacity(num_values);
        for _ in 0..num_values {
            values.push(self.leaf_field(type_name)?);
        }

        Ok(Value::Array(values))
    }

    fn should_be_null(&mut self) -> bool {
        if let Some((numerator, denominator)) = self.cfg.null_ratio {
            self.rng.random_ratio(numerator, denominator)
        } else {
            false
        }
    }
}
