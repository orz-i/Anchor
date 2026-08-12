use serde_json::{json, Value};

use crate::skills::SkillCatalog;

pub(crate) const IMAGE_VIEWER_RESOURCE_URI: &str = "ui://anchor/image-viewer/v1.html";
const IMAGE_VIEWER_MIME_TYPE: &str = "text/html;profile=mcp-app";

pub(crate) fn image_viewer_tool_meta(invoking: &str, invoked: &str) -> Value {
    json!({
        "ui": {
            "resourceUri": IMAGE_VIEWER_RESOURCE_URI,
            "visibility": ["model", "app"]
        },
        "openai/outputTemplate": IMAGE_VIEWER_RESOURCE_URI,
        "openai/widgetAccessible": true,
        "openai/toolInvocation/invoking": invoking,
        "openai/toolInvocation/invoked": invoked
    })
}

pub(crate) fn resources_list(catalog: &SkillCatalog, params: &Value) -> Result<Value, Value> {
    let first_page = params
        .get("cursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .is_none();
    let mut result = crate::skills::resources_list(catalog, params)?;
    if first_page {
        let resources = result
            .get_mut("resources")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| {
                rpc_error(-32603, "resources/list returned an invalid resource array")
            })?;
        resources.insert(0, image_viewer_resource_descriptor());
    }
    Ok(result)
}

pub(crate) fn widget_domain_from_public_base_url(public_base_url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(public_base_url.trim()).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let origin = parsed.origin().ascii_serialization();
    (origin != "null").then_some(origin)
}

pub(crate) fn resource_read(
    catalog: &SkillCatalog,
    widget_domain: Option<&str>,
    params: &Value,
) -> Result<Value, Value> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_error(-32602, "Missing resource uri"))?;
    if uri == IMAGE_VIEWER_RESOURCE_URI {
        let mut metadata = json!({
            "ui": {
                "prefersBorder": true,
                "csp": {
                    "connectDomains": [],
                    "resourceDomains": []
                }
            }
        });
        if let Some(domain) = widget_domain.filter(|domain| !domain.trim().is_empty()) {
            metadata["ui"]["domain"] = Value::String(domain.to_string());
            metadata["openai/widgetDomain"] = Value::String(domain.to_string());
        }
        return Ok(json!({
            "contents": [{
                "uri": IMAGE_VIEWER_RESOURCE_URI,
                "mimeType": IMAGE_VIEWER_MIME_TYPE,
                "text": IMAGE_VIEWER_HTML,
                "_meta": metadata
            }]
        }));
    }
    crate::skills::resource_read(catalog, params)
}

fn image_viewer_resource_descriptor() -> Value {
    json!({
        "uri": IMAGE_VIEWER_RESOURCE_URI,
        "name": "Anchor image viewer",
        "title": "Anchor image results",
        "description": "Inline MCP Apps image viewer for workspace images and Browser screenshots.",
        "mimeType": IMAGE_VIEWER_MIME_TYPE
    })
}

fn rpc_error(code: i32, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

const IMAGE_VIEWER_HTML: &str = r###"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <title>Anchor image viewer</title>
  <style>
    :root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    * { box-sizing: border-box; }
    body { margin: 0; background: transparent; color: CanvasText; }
    .card { display: grid; gap: 10px; padding: 10px; min-width: 0; }
    .stage { min-height: 120px; display: grid; place-items: center; overflow: hidden; border-radius: 10px; background: color-mix(in srgb, CanvasText 5%, Canvas); }
    img { display: block; max-width: 100%; max-height: min(70vh, 720px); object-fit: contain; }
    .empty { padding: 28px 14px; text-align: center; color: GrayText; font-size: 13px; }
    .footer { display: flex; align-items: center; justify-content: space-between; gap: 10px; min-width: 0; }
    .meta { min-width: 0; font-size: 12px; color: GrayText; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    button { border: 1px solid color-mix(in srgb, CanvasText 18%, transparent); background: Canvas; color: CanvasText; border-radius: 8px; padding: 6px 10px; font: inherit; cursor: pointer; }
    button[hidden] { display: none; }
  </style>
</head>
<body>
  <main class="card">
    <section class="stage" aria-live="polite">
      <img id="image" alt="Image returned by Anchor" hidden />
      <div id="empty" class="empty">Waiting for image result…</div>
    </section>
    <div class="footer">
      <div id="meta" class="meta">Anchor image result</div>
      <button id="fullscreen" type="button" hidden>Fullscreen</button>
    </div>
  </main>
  <script>
    (() => {
      const image = document.getElementById("image");
      const empty = document.getElementById("empty");
      const meta = document.getElementById("meta");
      const fullscreen = document.getElementById("fullscreen");
      const pending = new Map();
      let nextRequestId = 1;
      let objectUrl = null;
      let previewPath = null;
      let previewRequested = false;

      const request = (method, params) => {
        const id = nextRequestId++;
        window.parent.postMessage({ jsonrpc: "2.0", id, method, params }, "*");
        return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
      };

      const unwrapResult = (value) => {
        if (!value || typeof value !== "object") return value;
        if (Array.isArray(value.content) || value.structuredContent) return value;
        if (value.mcp_tool_result) return unwrapResult(value.mcp_tool_result);
        if (value.call_tool_result) return unwrapResult(value.call_tool_result);
        if (value.result) return unwrapResult(value.result);
        return value;
      };

      const structuredOf = (result) => {
        const unwrapped = unwrapResult(result);
        return unwrapped?.structuredContent || window.openai?.toolOutput || null;
      };

      const imageContentOf = (result) => {
        const unwrapped = unwrapResult(result);
        return Array.isArray(unwrapped?.content)
          ? unwrapped.content.find((item) => item?.type === "image" && typeof item.data === "string")
          : null;
      };

      const artifactPathOf = (structured) => {
        const artifacts = structured?.workspace_artifacts || structured?.result?.workspace_artifacts;
        if (!Array.isArray(artifacts)) return null;
        const artifact = artifacts.find((item) =>
          item?.exists === true && item?.kind === "file" && typeof item.workspace_path === "string" &&
          /\.(png|jpe?g|webp|gif)$/i.test(item.workspace_path)
        );
        return artifact?.workspace_path || null;
      };

      const decodeImage = (item) => {
        const raw = atob(item.data);
        const bytes = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
        return new Blob([bytes], { type: item.mimeType || item.mime_type || "image/png" });
      };

      const describe = (structured, item) => {
        const parts = [];
        const path = structured?.path || previewPath;
        if (path) parts.push(path);
        const width = structured?.width || image.naturalWidth;
        const height = structured?.height || image.naturalHeight;
        if (width && height) parts.push(`${width} × ${height}`);
        const mime = structured?.mime_type || item?.mimeType || item?.mime_type;
        if (mime) parts.push(mime.replace("image/", "").toUpperCase());
        const bytes = structured?.bytes;
        if (Number.isFinite(bytes)) parts.push(bytes >= 1048576 ? `${(bytes / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`);
        meta.textContent = parts.join(" · ") || "Anchor image result";
      };

      const showImage = (item, structured) => {
        if (objectUrl) URL.revokeObjectURL(objectUrl);
        objectUrl = URL.createObjectURL(decodeImage(item));
        image.onload = () => describe(structured, item);
        image.src = objectUrl;
        image.hidden = false;
        empty.hidden = true;
        fullscreen.hidden = typeof window.openai?.requestDisplayMode !== "function";
        describe(structured, item);
      };

      const loadWorkspacePreview = async (path) => {
        if (!path || previewRequested) return;
        previewRequested = true;
        previewPath = path;
        empty.textContent = `Loading saved screenshot ${path}…`;
        try {
          const result = await request("tools/call", {
            name: "view_image",
            arguments: { path, output: "mcp_image" }
          });
          render(result);
        } catch (error) {
          empty.textContent = `Screenshot saved to ${path}. Preview could not be loaded.`;
          meta.textContent = path;
        }
      };

      const render = (result) => {
        const unwrapped = unwrapResult(result);
        const structured = structuredOf(unwrapped);
        const item = imageContentOf(unwrapped);
        if (item) {
          showImage(item, structured);
          return;
        }
        const artifactPath = artifactPathOf(structured);
        if (artifactPath) {
          loadWorkspacePreview(artifactPath);
          return;
        }
        if (structured?.error_message || structured?.error?.message) {
          empty.textContent = structured.error_message || structured.error.message;
        }
        describe(structured, null);
      };

      window.addEventListener("message", (event) => {
        if (event.source !== window.parent) return;
        const message = event.data;
        if (!message || message.jsonrpc !== "2.0") return;
        if (message.id !== undefined && pending.has(message.id)) {
          const waiter = pending.get(message.id);
          pending.delete(message.id);
          if (message.error) waiter.reject(message.error);
          else waiter.resolve(message.result);
          return;
        }
        if (message.method === "ui/notifications/tool-result") render(message.params);
      }, { passive: true });

      fullscreen.addEventListener("click", async () => {
        try { await window.openai?.requestDisplayMode?.({ mode: "fullscreen" }); } catch (_) {}
      });

      const compatibility = window.openai?.toolResponseMetadata;
      if (compatibility) render(compatibility.mcp_tool_result || compatibility.call_tool_result || compatibility);
    })();
  </script>
</body>
</html>"###;

#[cfg(test)]
mod tests {
    use super::{
        image_viewer_tool_meta, resource_read, resources_list, widget_domain_from_public_base_url,
        IMAGE_VIEWER_RESOURCE_URI,
    };
    use crate::skills::{SkillCatalog, SkillSettings};
    use serde_json::json;

    #[test]
    fn image_viewer_resource_is_available_when_skills_are_disabled() {
        let root = tempfile::tempdir().expect("workspace");
        let catalog = SkillCatalog::new(root.path().to_path_buf());
        catalog.configure(SkillSettings::from_text(false, "skills"));

        let listed = resources_list(&catalog, &json!({})).expect("resources/list");
        assert_eq!(listed["resources"][0]["uri"], IMAGE_VIEWER_RESOURCE_URI);

        let read = resource_read(
            &catalog,
            Some("https://anchor.taoyan.icu"),
            &json!({"uri": IMAGE_VIEWER_RESOURCE_URI}),
        )
        .expect("resources/read");
        assert_eq!(read["contents"][0]["mimeType"], "text/html;profile=mcp-app");
        assert_eq!(
            read["contents"][0]["_meta"]["ui"]["domain"],
            "https://anchor.taoyan.icu"
        );
        assert_eq!(
            read["contents"][0]["_meta"]["openai/widgetDomain"],
            "https://anchor.taoyan.icu"
        );
        let html = read["contents"][0]["text"].as_str().expect("html");
        assert!(html.contains("ui/notifications/tool-result"));
        assert!(html.contains("toolResponseMetadata"));
        assert!(html.contains("name: \"view_image\""));
    }

    #[test]
    fn image_viewer_tool_metadata_uses_standard_and_chatgpt_aliases() {
        let metadata = image_viewer_tool_meta("Loading…", "Ready");
        assert_eq!(metadata["ui"]["resourceUri"], IMAGE_VIEWER_RESOURCE_URI);
        assert_eq!(metadata["ui"]["visibility"], json!(["model", "app"]));
        assert_eq!(metadata["openai/outputTemplate"], IMAGE_VIEWER_RESOURCE_URI);
        assert_eq!(metadata["openai/widgetAccessible"], true);
    }

    #[test]
    fn widget_domain_uses_https_public_origin_without_path() {
        assert_eq!(
            widget_domain_from_public_base_url("https://anchor.taoyan.icu/mcp?x=1"),
            Some("https://anchor.taoyan.icu".into())
        );
        assert_eq!(
            widget_domain_from_public_base_url("https://gateway.example.com:8443/w/anchor"),
            Some("https://gateway.example.com:8443".into())
        );
        assert_eq!(
            widget_domain_from_public_base_url("http://127.0.0.1:28766"),
            None
        );
    }
}
