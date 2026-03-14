use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use crate::subsonic::params::ResponseFormat;

/// Wrap a subsonic response body and serialize to the client's requested format.
pub struct SubsonicResponse {
    pub format: ResponseFormat,
    pub body: Value,
}

impl SubsonicResponse {
    /// Create a successful response with the given body fields merged into the
    /// standard subsonic-response wrapper.
    pub fn ok(format: ResponseFormat, body: Value) -> Self {
        Self { format, body }
    }

    /// Create an empty successful response (e.g., for ping).
    pub fn empty(format: ResponseFormat) -> Self {
        Self {
            format,
            body: json!({}),
        }
    }
}

impl IntoResponse for SubsonicResponse {
    fn into_response(self) -> Response {
        match self.format {
            ResponseFormat::Json | ResponseFormat::Jsonp(_) => {
                let mut wrapper = json!({
                    "subsonic-response": {
                        "status": "ok",
                        "version": "1.16.1",
                        "type": "fugue",
                        "serverVersion": "0.1.0",
                        "openSubsonic": true
                    }
                });

                // Merge body fields into subsonic-response
                if let Some(body_obj) = self.body.as_object() {
                    if let Some(inner) = wrapper.get_mut("subsonic-response") {
                        if let Some(inner_obj) = inner.as_object_mut() {
                            for (k, v) in body_obj {
                                inner_obj.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }

                let json_str = serde_json::to_string(&wrapper).unwrap_or_default();

                match self.format {
                    ResponseFormat::Jsonp(ref callback) => (
                        [("content-type", "application/javascript; charset=utf-8")],
                        format!("{callback}({json_str})"),
                    )
                        .into_response(),
                    _ => (
                        [("content-type", "application/json; charset=utf-8")],
                        json_str,
                    )
                        .into_response(),
                }
            }
            ResponseFormat::Xml => {
                let xml = build_xml_response(&self.body);
                (
                    [("content-type", "text/xml; charset=utf-8")],
                    xml,
                )
                    .into_response()
            }
        }
    }
}

fn build_xml_response(body: &Value) -> String {
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push('\n');
    xml.push_str(r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.16.1" type="fugue" serverVersion="0.1.0" openSubsonic="true">"#);

    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            json_to_xml(&mut xml, key, value, 1);
        }
    }

    xml.push_str("</subsonic-response>");
    xml
}

fn json_to_xml(xml: &mut String, tag: &str, value: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    match value {
        Value::Array(arr) => {
            for item in arr {
                json_to_xml(xml, tag, item, depth);
            }
        }
        Value::Object(obj) => {
            xml.push('\n');
            xml.push_str(&indent);
            xml.push('<');
            xml.push_str(tag);

            // Separate attributes (primitives) from child elements (objects/arrays)
            let mut children = Vec::new();
            for (k, v) in obj {
                match v {
                    Value::Object(_) | Value::Array(_) => {
                        children.push((k, v));
                    }
                    _ => {
                        xml.push(' ');
                        xml.push_str(k);
                        xml.push_str("=\"");
                        xml.push_str(&xml_escape(&primitive_to_string(v)));
                        xml.push('"');
                    }
                }
            }

            if children.is_empty() {
                xml.push_str("/>");
            } else {
                xml.push('>');
                for (k, v) in children {
                    json_to_xml(xml, k, v, depth + 1);
                }
                xml.push('\n');
                xml.push_str(&indent);
                xml.push_str("</");
                xml.push_str(tag);
                xml.push('>');
            }
        }
        _ => {
            xml.push('\n');
            xml.push_str(&indent);
            xml.push('<');
            xml.push_str(tag);
            xml.push('>');
            xml.push_str(&xml_escape(&primitive_to_string(value)));
            xml.push_str("</");
            xml.push_str(tag);
            xml.push('>');
        }
    }
}

fn primitive_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => v.to_string(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
