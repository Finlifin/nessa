//! Execute source fixtures and assert an integer result rather than only success.

pub fn run_value(source: &str) -> Result<i64, String> {
    run_number(source)?
        .to_i64_checked()
        .ok_or_else(|| "result does not fit i64".to_owned())
}

pub fn run_number(source: &str) -> Result<runtime::Number, String> {
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    let artifact = result.into_artifact().map_err(|error| error.to_string())?;
    let mut engine = initialization::Engine::with_defaults();
    let entry =
        driver::install_artifact(engine.vm_mut(), artifact).map_err(|error| error.to_string())?;
    let task = engine
        .vm_mut()
        .spawn_root(entry.expect("fixture has a main function"));
    match engine.vm_mut().run() {
        interpreter::VmResult::Finished => {
            assert_eq!(engine.vm.active_stack_count(), 0);
            engine
                .vm_mut()
                .task_result_number(task)
                .map_err(|error| format!("{error:?}"))
        }
        interpreter::VmResult::Error(error) => Err(format!("{error:?}")),
    }
}
