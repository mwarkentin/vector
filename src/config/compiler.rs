use std::collections::HashSet;

use indexmap::{IndexMap, IndexSet};

use super::{
    builder::ConfigBuilder, graph::Graph, id::Inputs, schema, validation, ComponentKey, Config,
    OutputId, SourceConfig, TransformConfig,
};

/// to handle the expansions when building the graph we need to be able to get the list of inputs
/// that will replace a single input, as a String.
pub(crate) fn to_string_expansions(
    input: &IndexMap<ComponentKey, Vec<ComponentKey>>,
) -> IndexMap<String, Vec<String>> {
    input
        .iter()
        .map(|(key, values)| {
            let key: String = key.id().to_string();
            let values: Vec<String> = values.iter().map(|value| value.id().to_string()).collect();
            (key, values)
        })
        .collect::<IndexMap<_, _>>()
}

pub fn compile(mut builder: ConfigBuilder) -> Result<(Config, Vec<String>), Vec<String>> {
    let mut errors = Vec::new();

    // component names should not have dots in the configuration file
    // but components can expand (like route) to have components with a dot
    // so this check should be done before expanding components
    if let Err(name_errors) = validation::check_names(
        builder
            .transforms
            .keys()
            .chain(builder.sources.keys())
            .chain(builder.sinks.keys()),
    ) {
        errors.extend(name_errors);
    }

    let expansions = expand_macros(&mut builder)?;

    expand_globs(&mut builder);

    if let Err(type_errors) = validation::check_shape(&builder) {
        errors.extend(type_errors);
    }

    if let Err(type_errors) = validation::check_resources(&builder) {
        errors.extend(type_errors);
    }

    if let Err(output_errors) = validation::check_outputs(&builder) {
        errors.extend(output_errors);
    }

    #[cfg(feature = "enterprise")]
    let hash = Some(builder.sha256_hash());

    #[cfg(not(feature = "enterprise"))]
    let hash = None;

    let ConfigBuilder {
        global,
        #[cfg(feature = "api")]
        api,
        schema,
        #[cfg(feature = "enterprise")]
        enterprise,
        healthchecks,
        enrichment_tables,
        sources,
        sinks,
        transforms,
        tests,
        provider: _,
        secret,
    } = builder;

    let str_expansions = to_string_expansions(&expansions);
    let graph = match Graph::new(&sources, &transforms, &sinks, &str_expansions, schema) {
        Ok(graph) => graph,
        Err(graph_errors) => {
            errors.extend(graph_errors);
            return Err(errors);
        }
    };

    if let Err(type_errors) = graph.typecheck() {
        errors.extend(type_errors);
    }

    if let Err(e) = graph.check_for_cycles() {
        errors.push(e);
    }

    // Inputs are resolved from string into OutputIds as part of graph construction, so update them
    // here before adding to the final config (the types require this).
    let sinks = sinks
        .into_iter()
        .map(|(key, sink)| {
            let inputs = graph.inputs_for(&key);
            (key, sink.with_inputs(inputs))
        })
        .collect();
    let transforms = transforms
        .into_iter()
        .map(|(key, transform)| {
            let inputs = graph.inputs_for(&key);
            (key, transform.with_inputs(inputs))
        })
        .collect();
    let tests = tests
        .into_iter()
        .map(|test| test.resolve_outputs(&graph, &str_expansions))
        .collect::<Result<Vec<_>, Vec<_>>>()?;

    if errors.is_empty() {
        let mut config = Config {
            global,
            #[cfg(feature = "api")]
            api,
            schema,
            #[cfg(feature = "enterprise")]
            enterprise,
            hash,
            healthchecks,
            enrichment_tables,
            sources,
            sinks,
            transforms,
            tests,
            expansions,
            secret,
        };

        config.propagate_acknowledgements()?;

        let warnings = validation::warnings(&config);

        Ok((config, warnings))
    } else {
        Err(errors)
    }
}

/// Some component configs can act like macros and expand themselves into multiple replacement
/// configs. Performs those expansions and records the relevant metadata.
pub(super) fn expand_macros(
    config: &mut ConfigBuilder,
) -> Result<IndexMap<ComponentKey, Vec<ComponentKey>>, Vec<String>> {
    let mut expanded_transforms = IndexMap::new();
    let mut expansions = IndexMap::new();
    let mut errors = Vec::new();
    let parent_types = HashSet::new();

    while let Some((key, transform)) = config.transforms.pop() {
        if let Err(error) = transform.expand(
            key,
            &parent_types,
            &mut expanded_transforms,
            &mut expansions,
        ) {
            errors.push(error);
        }
    }
    config.transforms = expanded_transforms;

    if !errors.is_empty() {
        Err(errors)
    } else {
        Ok(expansions)
    }
}

/// Expand globs in input lists
pub(crate) fn expand_globs(config: &mut ConfigBuilder) {
    let candidates = config
        .sources
        .iter()
        .flat_map(|(key, s)| {
            s.inner
                .outputs(config.schema.log_namespace())
                .into_iter()
                .map(|output| OutputId {
                    component: key.clone(),
                    port: output.port,
                })
        })
        .chain(config.transforms.iter().flat_map(|(key, t)| {
            t.inner
                .outputs(&schema::Definition::any())
                .into_iter()
                .map(|output| OutputId {
                    component: key.clone(),
                    port: output.port,
                })
        }))
        .map(|output_id| output_id.to_string())
        .collect::<IndexSet<String>>();

    for (id, transform) in config.transforms.iter_mut() {
        expand_globs_inner(&mut transform.inputs, &id.to_string(), &candidates);
    }

    for (id, sink) in config.sinks.iter_mut() {
        expand_globs_inner(&mut sink.inputs, &id.to_string(), &candidates);
    }
}

enum InputMatcher {
    Pattern(glob::Pattern),
    String(String),
}

impl InputMatcher {
    fn matches(&self, candidate: &str) -> bool {
        use InputMatcher::*;

        match self {
            Pattern(pattern) => pattern.matches(candidate),
            String(s) => s == candidate,
        }
    }
}

fn expand_globs_inner(inputs: &mut Inputs<String>, id: &str, candidates: &IndexSet<String>) {
    let raw_inputs = std::mem::take(inputs);
    for raw_input in raw_inputs {
        let matcher = glob::Pattern::new(&raw_input)
            .map(InputMatcher::Pattern)
            .unwrap_or_else(|error| {
                warn!(message = "Invalid glob pattern for input.", component_id = %id, %error);
                InputMatcher::String(raw_input.to_string())
            });
        let mut matched = false;
        for input in candidates {
            if matcher.matches(input) && input != id {
                matched = true;
                inputs.extend(Some(input.to_string()))
            }
        }
        // If it didn't work as a glob pattern, leave it in the inputs as-is. This lets us give
        // more accurate error messages about non-existent inputs.
        if !matched {
            inputs.extend(Some(raw_input))
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_util::mock::{basic_sink, basic_source, basic_transform};

    #[test]
    fn glob_expansion() {
        let mut builder = ConfigBuilder::default();
        builder.add_source("foo1", basic_source().1);
        builder.add_source("foo2", basic_source().1);
        builder.add_source("bar", basic_source().1);
        builder.add_transform("foos", &["foo*"], basic_transform("", 1.0));
        builder.add_sink("baz", &["foos*", "b*"], basic_sink(1).1);
        builder.add_sink("quix", &["*oo*"], basic_sink(1).1);
        builder.add_sink("quux", &["*"], basic_sink(1).1);

        let config = builder.build().expect("build should succeed");

        assert_eq!(
            config
                .transforms
                .get(&ComponentKey::from("foos"))
                .map(|item| without_ports(item.inputs.clone()))
                .unwrap(),
            vec![ComponentKey::from("foo1"), ComponentKey::from("foo2")]
        );
        assert_eq!(
            config
                .sinks
                .get(&ComponentKey::from("baz"))
                .map(|item| without_ports(item.inputs.clone()))
                .unwrap(),
            vec![ComponentKey::from("foos"), ComponentKey::from("bar")]
        );
        assert_eq!(
            config
                .sinks
                .get(&ComponentKey::from("quux"))
                .map(|item| without_ports(item.inputs.clone()))
                .unwrap(),
            vec![
                ComponentKey::from("foo1"),
                ComponentKey::from("foo2"),
                ComponentKey::from("bar"),
                ComponentKey::from("foos")
            ]
        );
        assert_eq!(
            config
                .sinks
                .get(&ComponentKey::from("quix"))
                .map(|item| without_ports(item.inputs.clone()))
                .unwrap(),
            vec![
                ComponentKey::from("foo1"),
                ComponentKey::from("foo2"),
                ComponentKey::from("foos")
            ]
        );
    }

    fn without_ports(outputs: Inputs<OutputId>) -> Vec<ComponentKey> {
        outputs
            .into_iter()
            .map(|output| {
                assert!(output.port.is_none());
                output.component
            })
            .collect()
    }
}
