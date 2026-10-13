//! Parse + render arbitrary template text over arbitrary values: errors are
//! fine, panics are bugs.
//!
//! Input layout: `<values yaml> \0 <template> [\0 <helper template>]`; without
//! a NUL the whole input is the template.
#![no_main]

use std::rc::Rc;

use helm_rs::template::{Engine, Renderable};
use helm_rs::{DeterministicEnv, Map, Value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut parts = text.splitn(3, '\0');
    let (values, main, helper) = match (parts.next(), parts.next(), parts.next()) {
        (Some(t), None, _) => ("", t, None),
        (Some(v), Some(t), h) => (v, t, h),
        _ => return,
    };
    let values = helm_rs::yaml::from_str(values).unwrap_or(Value::Nil);
    let values = if values.as_map().is_some() {
        values
    } else {
        Value::empty_map()
    };

    let mut top = Map::new();
    top.insert("Values".into(), values);
    let mut release = Map::new();
    release.insert("Name".into(), Value::str("release-name"));
    release.insert("Namespace".into(), Value::str("default"));
    top.insert("Release".into(), Value::map(release));
    let top = Value::map(top);

    let mut templates = vec![Renderable {
        name: "t/templates/t.yaml".into(),
        source: Rc::from(main),
        vals: top.clone(),
        base_path: "t/templates".into(),
    }];
    if let Some(h) = helper {
        templates.push(Renderable {
            name: "t/templates/_helpers.tpl".into(),
            source: Rc::from(h),
            vals: top,
            base_path: "t/templates".into(),
        });
    }
    let _ = Engine::new().render(&mut DeterministicEnv::default(), templates);
});
