use std::collections::HashMap;

use nu_protocol::engine::{EngineState, Stack};
use nu_protocol::{Config, PipelineData, ShellError, Value};

use crate::eval_block;

#[cfg(windows)]
const ENV_SEP: &str = ";";
#[cfg(not(windows))]
const ENV_SEP: &str = ":";

/// Translate environment variables from Strings to Values. Requires config to be already set up in
/// case the user defined custom env conversions in config.nu.
///
/// It returns Option instead of Result since we do want to translate all the values we can and
/// skip errors. This function is called in the main() so we want to keep running, we cannot just
/// exit.
pub fn convert_env_values(
    engine_state: &EngineState,
    stack: &mut Stack,
    config: &Config,
) -> Option<ShellError> {
    let mut new_env_vars = vec![];
    let mut error = None;

    for scope in &stack.env_vars {
        let mut new_scope = HashMap::new();

        for (name, val) in scope {
            if let Some(env_conv) = config.env_conversions.get(name) {
                if let Some((block_id, from_span)) = env_conv.from_string {
                    let val_span = match val.span() {
                        Ok(sp) => sp,
                        Err(e) => {
                            error = error.or(Some(e));
                            continue;
                        }
                    };

                    let block = engine_state.get_block(block_id);

                    if let Some(var) = block.signature.get_positional(0) {
                        let mut stack = stack.collect_captures(&block.captures);
                        if let Some(var_id) = &var.var_id {
                            stack.add_var(*var_id, val.clone());
                        }

                        let result = eval_block(
                            engine_state,
                            &mut stack,
                            block,
                            PipelineData::new(val_span),
                        );

                        match result {
                            Ok(data) => {
                                let val = data.into_value(val_span);
                                new_scope.insert(name.to_string(), val);
                            }
                            Err(e) => error = error.or(Some(e)),
                        }
                    } else {
                        error = error.or_else(|| {
                            Some(ShellError::MissingParameter(
                                "block input".into(),
                                from_span,
                            ))
                        });
                    }
                } else {
                    new_scope.insert(name.to_string(), val.clone());
                }
            } else {
                new_scope.insert(name.to_string(), val.clone());
            }
        }

        new_env_vars.push(new_scope);
    }

    stack.env_vars = new_env_vars;

    error
}

/// Translate one environment variable from Value to String
pub fn env_to_string(
    env_name: &str,
    value: Value,
    engine_state: &EngineState,
    stack: &mut Stack,
    config: &Config,
) -> Result<String, ShellError> {
    if let Some(env_conv) = config.env_conversions.get(env_name) {
        if let Some((block_id, to_span)) = env_conv.to_string {
            let block = engine_state.get_block(block_id);

            if let Some(var) = block.signature.get_positional(0) {
                let val_span = value.span()?;
                let mut stack = stack.collect_captures(&block.captures);

                if let Some(var_id) = &var.var_id {
                    stack.add_var(*var_id, value);
                }

                Ok(
                    // This one is OK to fail: We want to know if custom conversion is working
                    eval_block(engine_state, &mut stack, block, PipelineData::new(val_span))?
                        .into_value(val_span)
                        .as_string()?,
                )
            } else {
                Err(ShellError::MissingParameter("block input".into(), to_span))
            }
        } else {
            // Do not fail here. Must succeed, otherwise setting a non-string env var would constantly
            // throw errors when running externals etc.
            Ok(value.into_string(ENV_SEP, config))
        }
    } else {
        // Do not fail here. Must succeed, otherwise setting a non-string env var would constantly
        // throw errors when running externals etc.
        Ok(value.into_string(ENV_SEP, config))
    }
}

/// Translate all environment variables from Values to Strings
pub fn env_to_strings(
    engine_state: &EngineState,
    stack: &mut Stack,
    config: &Config,
) -> Result<HashMap<String, String>, ShellError> {
    let env_vars = stack.get_env_vars();
    let mut env_vars_str = HashMap::new();
    for (env_name, val) in env_vars {
        let val_str = env_to_string(&env_name, val, engine_state, stack, config)?;
        env_vars_str.insert(env_name, val_str);
    }

    Ok(env_vars_str)
}
