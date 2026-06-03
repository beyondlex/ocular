use crate::types::ComponentInfo;

#[allow(clippy::too_many_arguments)]
pub(crate) fn save_proxy_config(config_path: &std::path::Path, _components: &[ComponentInfo], protocol: &str, editing_idx: Option<usize>, name: &str, listen: &str, remote: &str, mode: &str, interface: &str) {
    let Ok(content) = std::fs::read_to_string(config_path) else { return };
    let Ok(mut doc) = content.parse::<toml_edit::DocumentMut>() else { return };

    let proxies = doc.entry("proxy").or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let toml_edit::Item::ArrayOfTables(arr) = proxies else { return };

    if let Some(idx) = editing_idx {
        // Edit existing
        if let Some(table) = arr.get_mut(idx) {
            table["name"] = toml_edit::value(name);
            table["protocol"] = toml_edit::value(protocol);
            table["remote"] = toml_edit::value(remote);
            if mode == "capture" {
                table["mode"] = toml_edit::value("capture");
                table["interface"] = toml_edit::value(interface);
                table.remove("listen");
            } else {
                table["listen"] = toml_edit::value(listen);
                table.remove("mode");
                table.remove("interface");
            }
        }
    } else {
        // Add new
        let mut table = toml_edit::Table::new();
        table["name"] = toml_edit::value(name);
        table["protocol"] = toml_edit::value(protocol);
        table["remote"] = toml_edit::value(remote);
        if mode == "capture" {
            table["mode"] = toml_edit::value("capture");
            table["interface"] = toml_edit::value(interface);
        } else {
            table["listen"] = toml_edit::value(listen);
        }
        arr.push(table);
    }

    let _ = std::fs::write(config_path, doc.to_string());
}

pub(crate) fn format_proxy_toml(name: &str, protocol: &str, listen: &str, remote: &str, mode: &str, interface: &str) -> String {
    let mut s = format!("[[proxy]]\nname = \"{}\"\nprotocol = \"{}\"\nremote = \"{}\"\n", name, protocol, remote);
    if mode == "capture" {
        s.push_str(&format!("mode = \"capture\"\ninterface = \"{}\"\n", interface));
    } else if !listen.is_empty() {
        s.push_str(&format!("listen = \"{}\"\n", listen));
    }
    s.push('\n');
    s
}

pub(crate) fn delete_proxy_from_config(config_path: &std::path::Path, name: &str) {
    let Ok(content) = std::fs::read_to_string(config_path) else { return };
    let Ok(mut doc) = content.parse::<toml_edit::DocumentMut>() else { return };

    let Some(toml_edit::Item::ArrayOfTables(arr)) = doc.get_mut("proxy") else { return };
    let idx = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name));
    if let Some(idx) = idx {
        arr.remove(idx);
    }

    let _ = std::fs::write(config_path, doc.to_string());
}
