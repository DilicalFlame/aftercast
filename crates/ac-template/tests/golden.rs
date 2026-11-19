//! Golden tests: template + context -> exact `description.txt`.
//! Regenerate expectations with `UPDATE_GOLDEN=1 cargo test -p ac-template`.

use std::fs;
use std::path::Path;

use ac_core::config::Readme;
use ac_template::Renderer;

#[test]
fn golden_descriptions() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let renderer = Renderer::new(&dir);
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut cases = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let template = entry.unwrap().path();
        if template.extension().is_none_or(|e| e != "md") {
            continue;
        }
        cases += 1;
        let ctx =
            serde_json::from_str(&fs::read_to_string(template.with_extension("json")).unwrap())
                .unwrap();
        let source = fs::read_to_string(&template).unwrap();
        let text = renderer
            .render(&source, &ctx, &Readme::default())
            .unwrap()
            .text;
        let expected_path = template.with_extension("txt");
        if update {
            fs::write(&expected_path, &text).unwrap();
        }
        let expected = fs::read_to_string(&expected_path).unwrap_or_default();
        assert_eq!(
            text,
            expected.replace("\r\n", "\n"),
            "{}",
            template.display()
        );
    }
    assert!(cases >= 2);
}
