//! `listmngr plugins`: what this build's plugins add, one JSON line each.
pub fn run() {
    for plugin in listmngr_pipeline::plugins::describe() {
        println!(
            "{}",
            serde_json::json!({
                "name": plugin.name,
                "version": plugin.version,
                "rules": plugin.rules,
                "links": plugin.links,
                "handlers": plugin.handlers,
                "pipelines": plugin.pipelines,
                "archivers": plugin.archivers,
            })
        );
    }
}
