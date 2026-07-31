//! Boundary facts that live in a document rather than in code.
//!
//! An OpenAPI file declares endpoints as data. It has no grammar worth compiling
//! and no symbols to extract, so it is not a `LanguageAdapter` — but it is a
//! producer, and the linker cannot tell the difference once the facts are in the
//! shared vocabulary. That is the "spec files are detectors too" half of #32.

use crate::cross::{http_key, CrossFacts, HandlerRef, Provides};

/// A file that carries cross-service facts without being code.
///
/// The whole obligation is `CrossFacts`; nothing here parses a language. A
/// detector that wanted a grammar would be a `LanguageAdapter` instead.
pub trait SpecDetector: Send + Sync {
    fn id(&self) -> &'static str;
    /// File names this detector claims, as `*.suffix` or an exact name.
    fn file_globs(&self) -> &'static [&'static str];
    /// Cheap check on the text before the expensive parse: a repository holds far
    /// more YAML than it holds API specs.
    fn looks_like_one(&self, text: &str) -> bool;
    fn facts(&self, text: &str) -> CrossFacts;
}

/// One line per spec format, the same shape as the language registry.
pub fn registry() -> Vec<Box<dyn SpecDetector>> {
    vec![Box::new(OpenApi)]
}

/// The detector claiming `path`, if any.
pub fn detector_for<'a>(
    registry: &'a [Box<dyn SpecDetector>],
    path: &std::path::Path,
) -> Option<&'a dyn SpecDetector> {
    let name = path.file_name()?.to_str()?;
    registry
        .iter()
        .find(|d| {
            d.file_globs().iter().any(|g| match g.strip_prefix('*') {
                Some(suffix) => name.ends_with(suffix),
                None => *g == name,
            })
        })
        .map(AsRef::as_ref)
}

/// OpenAPI 3 and Swagger 2. One detector: the difference is a version key and a
/// `basePath`, not a different document.
struct OpenApi;

impl SpecDetector for OpenApi {
    fn id(&self) -> &'static str {
        "openapi"
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.yaml", "*.yml", "*.json"]
    }

    /// Every repository is full of YAML that is not an API description, and
    /// parsing all of it to find out would be the cost of this feature. The two
    /// version keys are the document's own declaration of what it is.
    fn looks_like_one(&self, text: &str) -> bool {
        text.lines().take(40).any(|l| {
            let l = l.trim_start();
            l.starts_with("openapi:") || l.starts_with("swagger:") || l.starts_with("\"openapi\"")
        })
    }

    fn facts(&self, text: &str) -> CrossFacts {
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
            return CrossFacts::default(); // malformed: no facts, not a failure
        };
        // Swagger 2 puts a prefix on every path; OpenAPI 3 puts it in `servers`,
        // where it is often a whole URL and belongs to deployment, not to the route
        let base = doc
            .get("basePath")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("");
        let Some(paths) = doc.get("paths").and_then(serde_yaml::Value::as_mapping) else {
            return CrossFacts::default();
        };

        let mut provides = Vec::new();
        for (path, methods) in paths {
            let (Some(path), Some(methods)) = (path.as_str(), methods.as_mapping()) else {
                continue;
            };
            for (method, _operation) in methods {
                let Some(method) = method.as_str() else {
                    continue;
                };
                if !VERBS.contains(&method.to_lowercase().as_str()) {
                    continue; // `parameters`, `summary` and friends sit beside the verbs
                }
                provides.push(Provides {
                    key: http_key(method, &format!("{base}{path}")),
                    // the document declares the endpoint; it does not implement it.
                    // Naming the generated handler would mean guessing which of
                    // several code generators produced it and how it spells names
                    handler: HandlerRef::Here,
                    // a data file has no enclosing symbol to attribute to
                    line: 0,
                    returns: None,
                });
            }
        }
        CrossFacts {
            provides,
            ..Default::default()
        }
    }
}

const VERBS: [&str; 7] = ["get", "put", "post", "delete", "options", "head", "patch"];

#[cfg(test)]
mod tests {
    use super::*;
    use ir::Segment;

    const SPEC: &str = r#"
openapi: 3.0.0
info:
  title: t
paths:
  /health:
    get:
      operationId: Health
  /api/v1/users/{id}:
    get:
      operationId: GetUser
    delete:
      operationId: DeleteUser
    parameters:
      - name: id
"#;

    fn keys(text: &str) -> Vec<(String, Vec<Segment>)> {
        OpenApi
            .facts(text)
            .provides
            .into_iter()
            .map(|p| (p.key.method.unwrap_or_default(), p.key.path))
            .collect()
    }

    #[test]
    fn paths_and_verbs_become_route_keys() {
        let lit = |s: &str| Segment::Literal(s.to_owned());
        assert_eq!(
            keys(SPEC),
            vec![
                ("GET".into(), vec![lit("health")]),
                (
                    "GET".into(),
                    vec![lit("api"), lit("v1"), lit("users"), Segment::Param]
                ),
                (
                    "DELETE".into(),
                    vec![lit("api"), lit("v1"), lit("users"), Segment::Param]
                ),
            ],
            "a braced segment is a parameter, and `parameters:` beside the verbs is not one"
        );
    }

    /// Swagger 2 prefixes every path with `basePath`; OpenAPI 3 does not.
    #[test]
    fn swagger_two_carries_its_base_path() {
        let spec = "swagger: \"2.0\"\nbasePath: /api\npaths:\n  /users:\n    get: {}\n";
        let lit = |s: &str| Segment::Literal(s.to_owned());
        assert_eq!(
            keys(spec),
            vec![("GET".into(), vec![lit("api"), lit("users")])]
        );
    }

    #[test]
    fn only_a_document_that_says_what_it_is_gets_parsed() {
        assert!(OpenApi.looks_like_one(SPEC));
        assert!(OpenApi.looks_like_one("swagger: \"2.0\"\n"));
        assert!(!OpenApi.looks_like_one("name: ci\non: push\njobs:\n  build:\n"));
        // and a malformed one is no facts rather than an error
        assert!(OpenApi.facts("paths: [oh no").provides.is_empty());
    }
}
