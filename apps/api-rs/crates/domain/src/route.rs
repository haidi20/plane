// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! The Rust side of the route contract.
//!
//! `contracts/routes.json` is generated from Django\'s resolver and frozen in
//! git. Every route the Rust binary serves must be declared here first, in the
//! same order, so `contracts/tools/route_parity.py` can diff both directions.
//!
//! Path templates use Django spelling (`<uuid:pk>`, `<str:slug>`) so a diff
//! points at the exact segment that changed, not at axum syntax.

use serde::Serialize;

/// One path template with the HTTP methods it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSpec {
    /// Django-style template, trailing slash included.
    pub path: &'static str,
    /// Uppercase verbs, or `["*"]` for function-style views.
    pub methods: &'static [&'static str],
    /// Django `name=` when the route is reversed somewhere; otherwise `None`.
    pub name: Option<&'static str>,
    /// `false` while the handler is still a 501 stub.
    pub implemented: bool,
}

impl RouteSpec {
    /// Path with `{param}` placeholders, i.e. axum 0.8 syntax.
    #[must_use]
    pub fn axum_path(&self) -> String {
        to_axum(self.path)
    }
}

/// Convert a Django path template into an axum 0.8 one.
///
/// `<uuid:pk>` becomes `{pk}`; the converter moves into the extractor type, so
/// a bad UUID still fails at routing time (404) instead of inside a handler.
#[must_use]
pub fn to_axum(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        let end = start + end;
        out.push_str(&rest[..start]);
        let inner = &rest[start + 1..end];
        let name = inner.rsplit(':').next().unwrap_or(inner);
        out.push('{');
        out.push_str(name);
        out.push('}');
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Routes this binary answers today, in Django resolver order.
///
/// Kept tiny on purpose: the first slice is the health/robots door from
/// `plane/web/urls.py`. Adding a route here without a handler makes
/// `route-parity` fail, which is the point.
pub const ROUTES: &[RouteSpec] = &[
    RouteSpec {
        path: "robots.txt",
        methods: &["*"],
        name: None,
        implemented: true,
    },
    RouteSpec {
        path: "",
        methods: &["*"],
        name: None,
        implemented: true,
    },
];

/// Manifest payload consumed by `contracts/tools/route_parity.py`.
#[derive(Debug, Serialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub generated_by: &'static str,
    pub routes: Vec<ManifestRoute>,
}

/// One manifest entry, mirroring Django\'s `(path, methods)` pair.
#[derive(Debug, Serialize)]
pub struct ManifestRoute {
    pub path: String,
    pub methods: Vec<String>,
    pub name: Option<String>,
    pub order: usize,
    pub implemented: bool,
    pub axum_path: String,
}

/// Build the manifest for the routes declared above.
#[must_use]
pub fn manifest() -> Manifest {
    Manifest {
        schema_version: 1,
        generated_by: "plane-api --bin route-manifest",
        routes: ROUTES
            .iter()
            .enumerate()
            .map(|(index, route)| ManifestRoute {
                path: route.path.to_string(),
                methods: route.methods.iter().map(|m| (*m).to_string()).collect(),
                name: route.name.map(str::to_string),
                order: index + 1,
                implemented: route.implemented,
                axum_path: route.axum_path(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_uuid_and_str_segments() {
        assert_eq!(
            to_axum("api/workspaces/<str:slug>/projects/<uuid:pk>/"),
            "api/workspaces/{slug}/projects/{pk}/"
        );
    }

    #[test]
    fn keeps_literal_segments_untouched() {
        assert_eq!(to_axum("robots.txt"), "robots.txt");
        assert_eq!(to_axum(""), "");
    }
}
