use crate::{
    ftv1,
    handle::ByteResponse,
    state::{Config, FederatedSchema, State},
};
use anyhow::anyhow;
use apollo_compiler::{
    ExecutableDocument, Name, Node,
    ast::OperationType,
    executable::{Field, Operation},
    request::coerce_variable_values,
    response::JsonMap,
    validation::{Valid, WithErrors},
};
use apollo_configuration::{Validate, configuration};
use apollo_smith::{
    BooleanGenerator, FloatGenerator, Generator, Generators, IntGenerator, RandProvider,
    RandomProvider, ResponseBuilder, ResponseError, StringGenerator,
};
use cached::proc_macro::cached;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{
    HeaderMap, Response, StatusCode,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};
use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use rand::{RngExt, SeedableRng, rngs::StdRng, seq::IteratorRandom};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json_bytes::{
    ByteString, Map, Value, json,
    serde_json::{self},
};
use std::{
    collections::{BTreeMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    mem,
    sync::Arc,
};
use tracing::{debug, error, trace};

pub async fn handle(
    body_bytes: Vec<u8>,
    subgraph_name: Option<&str>,
    state: Arc<State>,
    should_emit_ftv1: bool,
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

    let config = state.config.read().await;
    let schema = state.schema.read().await;
    let rgen_cfg = subgraph_name
        .and_then(|name| config.subgraph_overrides.response_generation.get(name))
        .unwrap_or_else(|| &config.response_generation);

    // Since the response gen config and schema can be reloaded, they need to be included in the cache hash
    // alongside the query itself. This does mean that hot reloads will balloon memory over time since the old
    // values aren't invalidated. If we find this to actually be a practical problem in test scenarios that
    // demand a high cardinality of config/schema setups, we can set up more intelligent caching with invalidation.
    let mut hasher = DefaultHasher::new();
    req.query.hash(&mut hasher);
    rgen_cfg.hash(&mut hasher);
    schema.hash(&mut hasher);
    let cache_hash = hasher.finish();

    // The cached response bytes are time-independent, but a trace is per-request, so capture just
    // enough of the request to rebuild the trace after the (potentially cached) response bytes are
    // produced, rather than threading `req` itself through the cached path.
    let ftv1_req = should_emit_ftv1.then(|| GraphQLRequest {
        query: req.query.clone(),
        operation_name: req.operation_name.clone(),
        variables: JsonMap::new(),
    });

    // We draw exactly one RNG per request and thread it sequentially through  http-error injection, response
    // generation, and header injection. With a seeded `RngSource`, that gives stable aggregate counts under
    // concurrent dispatch.
    let mut rng = state.rng.next();

    if let Some(Ratio(numerator, denominator)) = rgen_cfg.http_error_ratio
        && rng.random_ratio(numerator, denominator)
    {
        return Response::builder()
            .status(rng.random_range(500..=504))
            .body(Empty::new().map_err(|never| match never {}).boxed())
            .map_err(|err| err.into());
    }

    let cache_enabled = subgraph_name
        .and_then(|name| config.subgraph_overrides.cache_responses.get(name).copied())
        .unwrap_or_else(|| config.cache_responses);

    let (bytes, status_code) = if cache_enabled {
        into_response_bytes_and_status_code(rgen_cfg, req, &schema, cache_hash, &mut rng).await
    } else {
        generate_body(rgen_cfg, req, &schema, cache_hash, &mut rng).await
    };

    // FTV1 traces are spliced in here, off the cached hot path, so cached bytes stay byte-for-byte
    // identical. Only 200 responses carry a trace; validation-error (400) and 5xx bodies are left
    // untouched.
    //
    // `splice_ftv1_trace` calls `parse_and_validate` again to recover the document, relying on it
    // already being populated: whichever branch above produced `bytes` (cached or not) internally
    // called `generate_body`, which calls `parse_and_validate(&req, schema, cache_hash)` with this
    // same `cache_hash`. Since that cache is keyed purely on `cache_hash` (see its `convert`
    // attribute), the call below is guaranteed a cache hit, not a fresh parse — this load-bearing
    // invariant is why `splice_ftv1_trace` doesn't need to handle a populate-on-miss cost itself.
    let bytes = match ftv1_req {
        Some(ftv1_req) if status_code == StatusCode::OK => {
            splice_ftv1_trace(bytes, &ftv1_req, &schema, cache_hash)
        }
        _ => bytes,
    };

    let mut resp = Response::new(Full::new(bytes).map_err(|never| match never {}).boxed());
    *resp.status_mut() = status_code;

    let headers = resp.headers_mut();
    add_headers(&config, rgen_cfg, subgraph_name, headers, &mut rng);

    Ok(resp)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQLRequest {
    pub query: String,
    pub operation_name: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "null_or_missing_as_default")]
    pub variables: JsonMap,
}

/// Allows a field to be either null *or* not present in a request. Some GraphQL implementations
/// specifically set variables to null rather than omitting them or providing an empty struct.
fn null_or_missing_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn add_headers(
    config: &Config,
    rgen_cfg: &ResponseGenerationConfig,
    subgraph_name: Option<&str>,
    headers: &mut HeaderMap,
    rng: &mut StdRng,
) {
    // HeaderMap is a multimap and yields Some(HeaderName) only for the first element of each multimap.
    // We have to track the last one we saw and treat that as the key for all subsequent None values as such.
    // Based on that contract, the first iteration will *always* yield a value so we can safely just initialize
    // this to a dummy value and trust that it will get overwritten instead of using an Option.
    let mut last_header_name: HeaderName = HeaderName::from_static("unused");
    let mut last_ratio: Option<Ratio> = None;

    for (header_name, header_value) in subgraph_name
        .and_then(|name| config.subgraph_overrides.headers.get(name).cloned())
        .unwrap_or_else(|| config.headers.clone())
        .into_iter()
    {
        if let Some(name) = header_name {
            last_ratio = rgen_cfg.header_ratio.get(name.as_str()).copied();
            last_header_name = name;
        }

        let should_insert = last_ratio
            .is_none_or(|Ratio(numerator, denominator)| rng.random_ratio(numerator, denominator));

        if should_insert {
            headers.insert(&last_header_name, header_value);
        }
    }

    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
}

/// Rebuilds the operation's trace and splices it into the response's `extensions.ftv1` field.
///
/// The document comes from the `parse_and_validate` cache and the node tree from
/// `cached_trace_shape`, so a cache hit only pays for pruning, encoding, and re-serializing. On any
/// failure the original bytes are returned unchanged so a trace can never break an otherwise-valid
/// response.
fn splice_ftv1_trace(
    bytes: Bytes,
    req: &GraphQLRequest,
    schema: &FederatedSchema,
    cache_hash: u64,
) -> Bytes {
    let Ok(doc) = parse_and_validate(req, schema, cache_hash) else {
        return bytes;
    };
    let Some(op) = primary_operation(&doc) else {
        return bytes;
    };

    let value: Value = match serde_json::from_slice(bytes.as_ref()) {
        Ok(value) => value,
        Err(err) => {
            error!(%err, "unable to parse response for ftv1 splicing");
            return bytes;
        }
    };

    // Cloned out before `value` is borrowed mutably below: `prune_to_response` needs a read-only
    // view of the actual generated data, independent of the `&mut` we take to splice `extensions`
    // in afterward.
    let data = value.get("data").cloned().unwrap_or(Value::Null);

    let mut value = value;
    let Some(response) = value.as_object_mut() else {
        return bytes;
    };

    let (shape, duration_ns) = cached_trace_shape(op, &doc, cache_hash);
    let mut trace = ftv1::Trace::from_shape(shape, duration_ns);
    // Errors before pruning: a field error drops its target key from `data` too (see
    // `generate_response`'s `to_drop`), so pruning first would remove the node before an error could
    // attach to it. `prune_to_response` spares error-carrying nodes for this reason.
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        populate_trace_errors(&mut trace, errors);
    }
    if let Some(root) = trace.root.as_mut() {
        prune_to_response(root, &[&data]);
    }
    let encoded = ftv1::encode_trace(&trace);

    let extensions = response
        .entry("extensions")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(extensions) = extensions.as_object_mut() {
        extensions.insert("ftv1", Value::String(encoded.into()));
    }

    match serde_json::to_vec(&value) {
        Ok(spliced) => spliced.into(),
        Err(err) => {
            error!(%err, "unable to re-serialize response with ftv1 trace");
            bytes
        }
    }
}

/// Populates `trace`'s error nodes from the response body's already-serialized `errors[]`, so the
/// trace and response agree on errors by construction. Mirrors Apollo Server's rule: a path-less
/// error (a whole-request failure) attaches to the root; a path-bearing error attaches to the
/// `root.child` with the matching `response_name`, falling back to root if the path doesn't resolve.
fn populate_trace_errors(trace: &mut ftv1::Trace, errors: &[Value]) {
    let Some(root) = trace.root.as_mut() else {
        return;
    };

    for error in errors {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let json = serde_json::to_string(error).unwrap_or_default();

        let response_name = error
            .get("path")
            .and_then(Value::as_array)
            .and_then(|path| path.first())
            .and_then(Value::as_str);

        let child_index = response_name.and_then(|name| {
            root.child
                .iter()
                .position(|child| child.response_name == name)
        });

        let target = match child_index {
            Some(index) => &mut root.child[index],
            None => &mut *root,
        };

        target.error.push(ftv1::Error {
            message,
            location: Vec::new(),
            time_ns: 0,
            json,
        });
    }
}

/// Prunes `node`'s subtree to the response keys actually present in `data`, dropping fields whose
/// interface/union fragment condition didn't match the concrete type apollo-smith resolved.
/// `ftv1::collect_fields` (used to build the cached shape) is type-condition-blind, so it can include
/// every fragment branch in the query; this walks the tree against the real response afterward and
/// drops whatever isn't there.
///
/// Kept separate from `cached_trace_shape` rather than folded into `TraceBuilder`: the shape is a
/// pure function of the query/schema and safe to cache unconditionally, but which fragment branch was
/// taken is random, per-request data — baking it into the cached tree would leak one request's
/// resolved types into every other request sharing its `cache_hash`.
///
/// Concrete (non-abstract) fields are unaffected, since apollo-smith always inserts every field it
/// resolves, nulls included — presence alone distinguishes "wrong fragment branch" from "legitimately
/// null". Must run after `populate_trace_errors`: a field error drops its target from `data` too, so
/// a node already carrying an error is kept regardless of what `data` says.
fn prune_to_response(node: &mut ftv1::Node, data: &[&Value]) {
    node.child
        .retain(|child| !child.error.is_empty() || field_present(data, &child.response_name));

    for child in &mut node.child {
        let child_data = child_values(data, &child.response_name);
        prune_to_response(child, &child_data);
    }
}

/// Whether `key` appears in at least one object among `data`. Empty `data` (an empty list, `null`,
/// or a scalar) means there's nothing to check against, so this returns `true` (keep, don't prune) —
/// the existing "no info" approximation, not a guess that the field is wrong.
fn field_present(data: &[&Value], key: &str) -> bool {
    let mut saw_object = false;
    for value in data {
        if let Some(object) = value.as_object() {
            saw_object = true;
            if object.contains_key(key) {
                return true;
            }
        }
    }

    !saw_object
}

/// Collects `key`'s value out of every object in `data`, flattening one level of array so a list
/// field's elements (merged into one child set by `ftv1::TraceBuilder`) feed the next level's
/// presence check together.
fn child_values<'a>(data: &[&'a Value], key: &str) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for value in data {
        let Some(child) = value.as_object().and_then(|object| object.get(key)) else {
            continue;
        };
        match child.as_array() {
            Some(items) => out.extend(items.iter()),
            None => out.push(child),
        }
    }

    out
}

/// The operation a request executes: the first operation defined in the document.
///
/// Both response generation (`generate_body`) and FTV1 trace generation (`splice_ftv1_trace`) call
/// this, so they're guaranteed to agree on which operation ran rather than merely computing the same
/// thing independently.
fn primary_operation(doc: &ExecutableDocument) -> Option<&Node<Operation>> {
    doc.operations.iter().next()
}

#[cached(result = true, key = "u64", convert = "{_cache_hash}")]
fn parse_and_validate(
    req: &GraphQLRequest,
    schema: &FederatedSchema,
    _cache_hash: u64,
) -> Result<Valid<ExecutableDocument>, WithErrors<ExecutableDocument>> {
    let op_name = req.operation_name.as_deref().unwrap_or("unknown");

    ExecutableDocument::parse_and_validate(schema, &req.query, op_name)
}

/// Builds (or reuses) the FTV1 node tree for an operation, keyed on the same `cache_hash` as the
/// response bytes and the parsed document.
///
/// Unlike the response bytes, this is safe to cache unconditionally — whether or not
/// `cache_responses` is enabled. `Trace::build_shape` reads only `op`/`doc`, which are already
/// captured by `cache_hash`, and never touches RNG state or generated response content, so the same
/// `cache_hash` always produces the same shape. What varies per request — errors, and the wall-clock
/// window from `Trace::from_shape` — is applied by the caller afterward, outside this cache.
///
/// Same memory caveat as `parse_and_validate`/`into_response_bytes_and_status_code`: entries are
/// never invalidated, so hot-reloading the schema/config repeatedly will grow this cache over time.
#[cached(key = "u64", convert = "{_cache_hash}")]
fn cached_trace_shape(
    op: &Node<Operation>,
    doc: &ExecutableDocument,
    _cache_hash: u64,
) -> (ftv1::Node, u64) {
    ftv1::Trace::build_shape(op, doc)
}

#[tracing::instrument(skip(req, schema, rng))]
#[cached(key = "u64", convert = "{cache_hash}")]
async fn into_response_bytes_and_status_code(
    cfg: &ResponseGenerationConfig,
    req: GraphQLRequest,
    schema: &FederatedSchema,
    cache_hash: u64,
    rng: &mut StdRng,
) -> (Bytes, StatusCode) {
    generate_body(cfg, req, schema, cache_hash, rng).await
}

#[tracing::instrument(skip(req, schema, rng))]
async fn generate_body(
    cfg: &ResponseGenerationConfig,
    req: GraphQLRequest,
    schema: &FederatedSchema,
    cache_hash: u64,
    rng: &mut StdRng,
) -> (Bytes, StatusCode) {
    debug!(%cache_hash, req.operation_name, "handling graphql request");
    trace!(variables=?req.variables, "request variables");

    let doc = match parse_and_validate(&req, schema, cache_hash) {
        Ok(doc) => doc,
        Err(err) => {
            let errs: Vec<_> = err.errors.iter().map(|d| d.to_json()).collect();
            error!(?errs, query=%req.query, "invalid graphql query");
            let bytes = serde_json::to_vec(&json!({ "data": Value::Null, "errors": errs }))
                .unwrap_or_default();
            return (bytes.into(), StatusCode::BAD_REQUEST);
        }
    };

    let op = primary_operation(&doc).unwrap();
    let op_name = op.name.as_ref().map(|name| name.as_str());

    debug!(
        ?op_name,
        type=%op.operation_type,
        n_selections = op.selection_set.selections.len(),
        "processing operation"
    );

    let resp = match op.operation_type {
        OperationType::Query => {
            match generate_response(cfg, op_name, &doc, schema, &req.variables, rng) {
                Ok(resp) => resp,
                Err(err) => {
                    error!(%err, "unable to generate response");
                    return (
                        Bytes::from("unable to generate response"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            }
        }

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
    schema: &FederatedSchema,
    variables: &JsonMap,
    rng: &mut StdRng,
) -> anyhow::Result<Value> {
    let op = match doc.operations.get(op_name) {
        Ok(op) => op,
        Err(_) => return Ok(json!({ "data": null })),
    };

    if let Some(Ratio(numerator, denominator)) = cfg.graphql_errors.request_error_ratio
        && rng.random_ratio(numerator, denominator)
    {
        return Ok(json!({
            "data": null,
            "errors": [{
                "message": "Request error simulated",
                "extensions": { "code": "INTERNAL_SERVER_ERROR" },
            }],
        }));
    }

    // Short-circuit introspection responses if a request is *only* introspection. This does mean that requests
    // that combine both introspection and non-introspection fields in their query will get random data for
    // the introspection fields. For our use-cases we only need correct introspection data if that is the only
    // data being requested, but if we want to make this fully spec-compliant in the future we will need to merge
    // the result of `partial_execute` with the random data generated on every query (which would be costlier).
    if op.is_introspection(doc) {
        return apollo_compiler::introspection::partial_execute(
            schema,
            &schema.implementers_map(),
            doc,
            op,
            &coerce_variable_values(schema, op, variables)
                .map_err(|err| anyhow!("{}", err.message()))?,
        )
        .map_err(|err| anyhow!("{}", err.message()))
        .and_then(|result| serde_json_bytes::to_value(result).map_err(|err| anyhow!("{}", err)));
    }

    let mut rng_provider = RandProvider(StdRng::from_rng(&mut *rng));
    let mut builder = ResponseBuilder::new(&mut rng_provider, doc, schema)
        .with_min_list_size(cfg.array.min_length)
        .with_max_list_size(cfg.array.max_length)
        .with_generator(
            Name::new_unchecked("_Service"),
            SdlOverride {
                sdl: schema.sdl().to_string(),
            },
        );

    if let Some(Ratio(numerator, denominator)) = cfg.null_ratio {
        builder = builder.with_null_ratio(numerator, denominator);
    }

    for (name, generator) in &cfg.scalars {
        builder = builder.with_generator(Name::new_unchecked(name.as_str()), *generator);
    }

    builder = builder.with_operation_name(op_name);

    let data = builder.build_data().map_err(|err| anyhow!("{}", err))?;

    // Select a random number of top-level fields to "fail" if we are going to have field errors. For the sake of
    // simplicity and performance, we won't traverse deeper into the response object.
    if let Some(Ratio(numerator, denominator)) = cfg.graphql_errors.field_error_ratio
        && rng.random_ratio(numerator, denominator)
    {
        let mut data = data.as_object().cloned().unwrap_or_default();
        let drop_count = rng.random_range(1..=data.len());
        let sampled_keys = data.keys().cloned().sample(rng, drop_count);
        let to_drop: HashSet<ByteString> = HashSet::from_iter(sampled_keys);

        data.retain(|key, _| !to_drop.contains(key));

        let errors: Vec<Value> = to_drop
            .into_iter()
            .map(|key| {
                json!({
                    "message": "Field error simulated",
                    "path": [key],
                    "extensions": { "code": "INTERNAL_SERVER_ERROR" },
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

/// A `(numerator, denominator)` ratio, e.g. `[1, 2]` for "1 in 2".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Ratio(pub u32, pub u32);

impl Validate for Ratio {}

#[configuration]
#[derive(Hash, Serialize)]
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

#[configuration]
#[derive(Hash, Serialize)]
pub struct ResponseGenerationConfig {
    #[config(default = default_scalar_config(), skip_validate)]
    pub scalars: BTreeMap<String, ScalarGenerator>,
    #[config(default = default_array_size())]
    pub array: ArraySize,
    #[config(default = default_null_ratio())]
    pub null_ratio: Option<Ratio>,
    #[config(skip_validate)]
    pub header_ratio: BTreeMap<String, Ratio>,
    pub http_error_ratio: Option<Ratio>,
    pub graphql_errors: GraphQLErrorConfig,
    pub ftv1: Option<bool>,
}

impl ResponseGenerationConfig {
    /// Merges the default scalar config with the provided config, allowing users to specify a partial set of scalar
    /// generators while inheriting the default configuration for those they do not specify.
    pub(crate) fn merge_default_scalars(&mut self) {
        let default = default_scalar_config();
        let provided = mem::replace(&mut self.scalars, default);
        self.scalars.extend(provided);
    }
}

fn default_scalar_config() -> BTreeMap<String, ScalarGenerator> {
    [
        ("Boolean".into(), ScalarGenerator::Bool),
        ("Int".into(), ScalarGenerator::Int { min: 0, max: 100 }),
        ("ID".into(), ScalarGenerator::Int { min: 0, max: 100 }),
        (
            "Float".into(),
            ScalarGenerator::Float {
                min: OrderedFloat(-1.0),
                max: OrderedFloat(1.0),
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
    Some(Ratio(1, 2))
}

// Kept as a hand-rolled (non-`#[configuration]`) type: `#[configuration]` enums are always
// externally tagged by variant name (e.g. `{int: {min: 0, max: 100}}`), with no way to opt into
// the internally-tagged `{type: int, min: 0, max: 100}` shape configured below
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Hash, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ScalarGenerator {
    Bool,
    Float {
        #[schemars(with = "f64")]
        min: OrderedFloat<f64>,
        #[schemars(with = "f64")]
        max: OrderedFloat<f64>,
    },
    Int {
        min: i32,
        max: i32,
    },
    String {
        min_len: usize,
        max_len: usize,
    },
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
}

impl<R: RandomProvider> Generator<R> for ScalarGenerator {
    fn generate(
        &mut self,
        rng: &mut R,
        generators: &mut Generators<R>,
        fields: &IndexMap<String, Vec<Node<Field>>>,
    ) -> Result<Value, ResponseError> {
        match *self {
            Self::Bool => BooleanGenerator.generate(rng, generators, fields),
            Self::Int { min, max } => IntGenerator { min, max }.generate(rng, generators, fields),
            Self::Float { min, max } => FloatGenerator {
                min: *min,
                max: *max,
            }
            .generate(rng, generators, fields),
            Self::String { min_len, max_len } => {
                StringGenerator { min_len, max_len }.generate(rng, generators, fields)
            }
        }
    }
}

/// Overrides generation of `_Service` so that `_service { sdl }` returns the original
/// supergraph SDL rather than a random string. Apollo Federation requires this exact
/// shape for the composition pipeline.
struct SdlOverride {
    sdl: String,
}

impl<R: RandomProvider> Generator<R> for SdlOverride {
    fn generate(
        &mut self,
        _rng: &mut R,
        _generators: &mut Generators<R>,
        fields: &IndexMap<String, Vec<Node<Field>>>,
    ) -> Result<Value, ResponseError> {
        let mut obj = Map::new();
        for (key, group) in fields {
            let value = match group[0].name.as_str() {
                "sdl" => Value::String(self.sdl.clone().into()),
                "__typename" => Value::String("_Service".into()),
                _ => Value::Null,
            };
            obj.insert(key.clone(), value);
        }
        Ok(Value::Object(obj))
    }
}

#[configuration]
#[derive(Copy, Hash, Serialize)]
pub struct ArraySize {
    #[config(required)]
    pub min_length: usize,
    #[config(required)]
    pub max_length: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn introspection_short_circuits() -> anyhow::Result<()> {
        let supergraph = include_str!("../../tests/data/schema.graphql");
        let schema = FederatedSchema::parse_string(supergraph, "../../tests/data/schema.graphql")?;

        let query = r#"
            query {
                __schema {
                    queryType {
                        name
                    }
                    types {
                        name
                        kind
                    }
                }
            }
        "#;

        let doc = ExecutableDocument::parse_and_validate(&schema, query, "query.graphql").unwrap();
        let cfg = ResponseGenerationConfig::default();
        let result = generate_response(
            &cfg,
            None,
            &doc,
            &schema,
            &JsonMap::new(),
            &mut StdRng::seed_from_u64(0),
        )?;

        assert!(result.get("data").is_some());
        let data = result.get("data").unwrap();
        assert!(data.get("__schema").is_some());
        // No other random data is included
        assert!(data.as_object().unwrap().len() == 1);

        let schema_obj = data.get("__schema").unwrap();
        assert!(schema_obj.get("queryType").is_some());

        let query_type = schema_obj.get("queryType").unwrap();
        assert_eq!(query_type.get("name").unwrap().as_str().unwrap(), "Query");

        let types = schema_obj.get("types").unwrap().as_array().unwrap();
        assert!(!types.is_empty());

        let type_names: Vec<&str> = types
            .iter()
            .filter_map(|t| t.get("name")?.as_str())
            .collect();
        assert!(type_names.contains(&"Query"));
        assert!(type_names.contains(&"User"));
        assert!(type_names.contains(&"Post"));

        Ok(())
    }

    #[test]
    fn service_introspection_uses_raw_schema() -> anyhow::Result<()> {
        let supergraph = include_str!("../../tests/data/schema.graphql");
        let schema = FederatedSchema::parse_string(supergraph, "../../tests/data/schema.graphql")?;

        let query = r#"
            query {
                _service {
                    sdl
                }
            }
        "#;

        let doc = ExecutableDocument::parse_and_validate(&schema, query, "query.graphql").unwrap();
        let cfg = ResponseGenerationConfig::default();
        let result = generate_response(
            &cfg,
            None,
            &doc,
            &schema,
            &JsonMap::new(),
            &mut StdRng::seed_from_u64(0),
        )?;

        assert!(result.get("data").is_some());
        let data = result.get("data").unwrap();
        assert!(data.get("_service").is_some());

        let schema_obj = data.get("_service").unwrap();
        assert!(schema_obj.get("sdl").is_some());

        let sdl = schema_obj.get("sdl").unwrap().as_str().unwrap();
        assert_eq!(supergraph, sdl);

        Ok(())
    }

    #[test]
    fn cached_trace_shape_reuses_cache_hash_regardless_of_document() -> anyhow::Result<()> {
        let supergraph = include_str!("../../tests/data/schema.graphql");
        let schema = FederatedSchema::parse_string(supergraph, "../../tests/data/schema.graphql")?;

        let doc_a =
            ExecutableDocument::parse_and_validate(&schema, r#"{ posts { title } }"#, "a.graphql")
                .unwrap();
        let doc_b = ExecutableDocument::parse_and_validate(
            &schema,
            r#"{ posts { title views } }"#,
            "b.graphql",
        )
        .unwrap();
        let op_a = primary_operation(&doc_a).unwrap();
        let op_b = primary_operation(&doc_b).unwrap();

        // A cache_hash value no other test/request could plausibly compute (real ones come from
        // hashing a query/config/schema), so this test can't collide with `cached_trace_shape`'s
        // process-global cache.
        const CACHE_HASH: u64 = 0x6f6c645f73686170;

        let (shape_a, _) = cached_trace_shape(op_a, &doc_a, CACHE_HASH);
        // Same cache_hash, a document that would build a different shape (`title` *and* `views`
        // instead of just `title`) if this weren't cached.
        let (shape_b, _) = cached_trace_shape(op_b, &doc_b, CACHE_HASH);

        assert_eq!(
            shape_a, shape_b,
            "same cache_hash should reuse the first-built shape rather than rebuilding from doc_b"
        );
        assert_eq!(shape_b.child.len(), 1);
        let posts = &shape_b.child[0];
        assert_eq!(
            posts.child.len(),
            1,
            "shape should still reflect doc_a's `{{ posts {{ title }} }}`, not doc_b's extra `views`"
        );
        assert_eq!(posts.child[0].response_name, "title");

        Ok(())
    }
}
