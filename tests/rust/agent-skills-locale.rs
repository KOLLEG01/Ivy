//! Reuse the Skills cases with a mandatory production runtime binding. The
//! general Agent domain suite retains its separate original fixture binding.
#[path = "agent-skills-files.rs"]
mod skills_file_cases;

fn skills_comparator() -> std::sync::Arc<ivy_agent_manager::skills_files::SkillsComparator> {
    let directory = std::env::var_os("IVY_TEST_LOCALE_RUNTIME")
        .expect("Provide the qualified packaged locale runtime with IVY_TEST_LOCALE_RUNTIME");
    let runtime =
        ivy_sdk::collation::LocaleRuntime::load(std::path::Path::new(&directory)).unwrap();
    let comparator = runtime.executing_default().unwrap();
    std::sync::Arc::new(move |left, right| comparator.compare(left, right))
}
