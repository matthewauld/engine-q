use nu_engine::CallExt;
use nu_protocol::{
    ast::Call, engine::Command, engine::EngineState, engine::Stack, Example, PipelineData,
    ShellError, Signature, Span, SyntaxShape, Value,
};

#[derive(Clone)]
pub struct Default;

impl Command for Default {
    fn name(&self) -> &str {
        "default"
    }

    fn signature(&self) -> Signature {
        Signature::build("default")
            .required("column name", SyntaxShape::String, "the name of the column")
            .required(
                "column value",
                SyntaxShape::Any,
                "the value of the column to default",
            )
    }

    fn usage(&self) -> &str {
        "Sets a default row's column if missing."
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<nu_protocol::PipelineData, ShellError> {
        default(engine_state, stack, call, input)
    }

    fn examples(&self) -> Vec<Example> {
        vec![Example {
            description: "Add the column 'Spam' with default value 3",
            example: r#"echo [["Hello" "World"]; [1 2]] | default Spam 3"#,
            result: Some(Value::List {
                vals: vec![Value::Record {
                    cols: vec!["Hello".into(), "World".into(), "Spam".into()],
                    vals: vec![Value::test_int(1), Value::test_int(2), Value::test_int(3)],
                    span: Span::test_data(),
                }],
                span: Span::test_data(),
            }),
        }]
    }
}

pub fn default(
    engine_state: &EngineState,
    stack: &mut Stack,
    call: &Call,
    input: PipelineData,
) -> Result<nu_protocol::PipelineData, ShellError> {
    let column: String = call.req(engine_state, stack, 0)?;
    let value: Value = call.req(engine_state, stack, 1)?;
    input.map(
        move |item| {
            if item.get_data_by_key(&column).is_some() {
                return item;
            }
            match item {
                Value::Record {
                    mut cols,
                    mut vals,
                    span,
                } => {
                    cols.push(column.clone());
                    vals.push(value.clone());
                    Value::Record { cols, vals, span }
                }
                _ => item,
            }
        },
        engine_state.ctrlc.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::Default;
    #[test]
    fn examples_work_as_expected() {
        use crate::test_examples;

        test_examples(Default {})
    }
}
