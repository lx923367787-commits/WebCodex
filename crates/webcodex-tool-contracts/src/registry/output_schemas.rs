use serde_json::Value;

mod agent_tasks;
mod artifacts;
mod checkpoints;
mod coding_agents;
mod goals;
mod host_goals;
mod coding_tasks;
mod common;
mod communication;
mod computer;
mod discovery;
mod documents;
mod edits;
mod files;
mod git;
mod hygiene;
mod jobs;
mod lsp;
mod memory;
mod projects;
mod runner_config;
mod sessions;
mod skills;
mod ssh_resources;
mod testing;

use common::default_output_schema;

pub fn output_schema_for_tool(name: &str) -> Value {
    let mut schema = base_output_schema_for_tool(name);
    add_context_meter_to_wrapped_output(&mut schema);
    schema
}

// Runtime context estimates are response metadata. Add them only at the tool
// output boundary, including closed conditional branches, never to nested data.
fn add_context_meter_to_wrapped_output(schema: &mut Value) {
    if let Some(output) = schema.pointer_mut("/properties/output") {
        add_context_meter_to_output_object(output);
    }
    for composition in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get_mut(composition).and_then(Value::as_array_mut) {
            for branch in branches {
                add_context_meter_to_wrapped_output(branch);
            }
        }
    }
    for conditional in ["if", "then", "else"] {
        if let Some(branch) = schema.get_mut(conditional) {
            add_context_meter_to_wrapped_output(branch);
        }
    }
}

fn add_context_meter_to_output_object(schema: &mut Value) {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert("context_meter".into(), host_goals::context_meter_schema());
    }
    for composition in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get_mut(composition).and_then(Value::as_array_mut) {
            for branch in branches {
                add_context_meter_to_output_object(branch);
            }
        }
    }
    for conditional in ["if", "then", "else"] {
        if let Some(branch) = schema.get_mut(conditional) {
            add_context_meter_to_output_object(branch);
        }
    }
}

fn base_output_schema_for_tool(name: &str) -> Value {
    if let Some(schema) = documents::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = agent_tasks::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = coding_agents::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = goals::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = host_goals::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = computer::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = communication::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = jobs::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = discovery::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = projects::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = runner_config::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = coding_tasks::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = checkpoints::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = artifacts::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = git::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = edits::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = sessions::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = memory::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = skills::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = hygiene::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = files::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = lsp::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = ssh_resources::output_schema_for_tool(name) {
        return schema;
    }
    if let Some(schema) = testing::output_schema_for_tool(name) {
        return schema;
    }

    default_output_schema()
}

#[cfg(any(test, feature = "root-test-support"))]
pub fn coding_workflow_diagnostic_output_schema_for_test() -> Value {
    coding_tasks::coding_workflow_diagnostic_output_schema()
}
