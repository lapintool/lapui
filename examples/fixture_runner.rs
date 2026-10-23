#[cfg(feature = "software-renderer")]
mod runner {
    use blitz::dom::Document;
    use blitz::traits::shell::{ColorScheme, Viewport};
    use lapui::{action::ActionRegistry, runtime::LapuiDocument};
    use serde_json::Value;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut output = PathBuf::from("target/fixture-comparison/lapui");
        let mut suite = String::from("basic");
        let (mut width, mut height) = (960_u32, 640_u32);
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            let value = arguments.next().ok_or("option requires a value")?;
            match argument.as_str() {
                "--out" => output = PathBuf::from(value),
                "--suite" => suite = value,
                "--width" => width = value.parse()?,
                "--height" => height = value.parse()?,
                _ => return Err(format!("unknown option: {argument}").into()),
            }
        }
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err("fixture dimensions must be 1..4096".into());
        }
        std::fs::create_dir_all(&output)?;
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = project.join("examples/ui-fixtures");
        let manifest: Value = serde_json::from_slice(&std::fs::read(root.join("manifest.json"))?)?;
        if !["basic", "frameworks", "all"].contains(&suite.as_str()) {
            return Err("suite must be basic, frameworks or all".into());
        }
        let mut scenarios = Vec::new();
        if suite != "frameworks" {
            for scenario in manifest["scenarios"]
                .as_array()
                .ok_or("missing scenarios")?
            {
                scenarios.push((
                    scenario.as_str().ok_or("invalid name")?.to_owned(),
                    root.join("index.html"),
                    format!("requestAnimationFrame(() => runFixture({scenario}));"),
                ));
            }
        }
        if suite != "basic" {
            let source = std::fs::read_to_string(root.join("frameworks.js"))?;
            for scenario in manifest["frameworkScenarios"]
                .as_array()
                .ok_or("missing framework scenarios")?
            {
                let name = scenario["name"].as_str().ok_or("invalid name")?.to_owned();
                let page = project.join(scenario["page"].as_str().ok_or("missing page")?);
                let script = format!(
                    "{source}\nrequestAnimationFrame(() => runFrameworkFixture({}));",
                    scenario["name"]
                );
                scenarios.push((name, page, script));
            }
        }
        let fonts = LapuiDocument::new_font_context();
        for (name, page, script) in scenarios {
            let html = std::fs::read_to_string(&page)?;
            let (mut document, _notify) = LapuiDocument::new_with_local_source_and_font_context(
                ActionRegistry::default(),
                None,
                &html,
                &script,
                page.parent().ok_or("page has no parent")?,
                fonts.clone(),
            )?;
            document.inner_mut().set_viewport(Viewport::new(
                width,
                height,
                1.0,
                ColorScheme::Light,
            ));
            let deadline = Instant::now() + Duration::from_secs(5);
            let result = loop {
                document.poll(None);
                document.animation_frame();
                document.rendering_update();
                let dom = document.inner();
                let text = dom
                    .query_selector("#fixture-result")
                    .ok()
                    .flatten()
                    .and_then(|id| dom.get_node(id))
                    .map(|node| node.text_content())
                    .unwrap_or_default();
                if !text.is_empty() {
                    break serde_json::from_str::<Value>(&text)?;
                }
                if Instant::now() >= deadline {
                    return Err(format!("fixture timed out: {name}").into());
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            std::fs::write(
                output.join(format!("{name}.json")),
                serde_json::to_vec_pretty(&result)?,
            )?;
            lapui::snapshot::save_png(
                &mut document,
                output.join(format!("{name}.png")),
                width,
                height,
            )?;
            if result["status"] != "ready"
                || result["checks"].as_object().is_none_or(|checks| {
                    checks.is_empty() || checks.values().any(|value| value != true)
                })
            {
                return Err(format!("fixture failed: {result}").into());
            }
            println!("{name}: ready ({width}x{height})");
        }
        Ok(())
    }
}

#[cfg(feature = "software-renderer")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    runner::run()
}

#[cfg(not(feature = "software-renderer"))]
fn main() {
    eprintln!("fixture_runner requires the software-renderer feature");
    std::process::exit(2);
}
