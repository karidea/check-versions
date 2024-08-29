#![deny(warnings)]
#![warn(rust_2018_idioms)]

use futures::prelude::*;
use std::collections::HashMap;
use std::env;
use std::str;
use std::fs;
use tokio;

use hyper::{body, Body, Client, Method, Request};
use hyper_tls::HttpsConnector;
use serde::Deserialize;
use clap::Parser;

#[derive(Deserialize, Debug)]
struct PackageJson {
    #[allow(unused)]
    dependencies: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize, Debug)]
struct Packages {
    #[allow(unused)]
    version: Option<String>,
}

#[derive(Deserialize, Debug)]
struct PackageLockJson {
    #[allow(unused)]
    packages: HashMap<String, Packages>,
}

#[derive(Deserialize, Debug)]
struct PackageLockJsonV1 {
    #[allow(unused)]
    dependencies: HashMap<String, Packages>,
}

#[derive(Deserialize, Debug)]
struct PartialPackageLockJson {
    #[allow(unused)]
    #[serde(rename = "lockfileVersion")]
    lockfile_version: Option<i32>,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Check versions of an npm package given list of repositories
#[derive(Parser, Debug, Clone)]
#[clap(version, about, long_about = None)]
struct Cli {
    /// Path of the file containing json list of repositories
    #[clap(short, long)]
    repos: String,

    /// Package name to check versions on
    #[clap(short, long)]
    package: String
}

const PARALLEL_REQUESTS: usize = 64;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let package_name = cli.package.as_str();
    let repos_path = cli.repos;

    let data = fs::read_to_string(&repos_path)
        .expect("Unable to read file");

    let json: Vec<String> = serde_json::from_str(&data)
        .expect("JSON does not have correct format.");

    let uris = json.iter().map(|repo| {
        let filename = "package-lock.json";
        let uri = format!("https://api.github.com/repos/{repo}/contents/{filename}");
        return uri;
    });

    let https = HttpsConnector::new();

    let client = Client::builder()
        .http2_only(true)
        .build::<_, hyper::Body>(https);

    let version_results = stream::iter(uris)
        .map(move |uri| {
            let request = Request::builder()
                .method(Method::GET)
                .uri(uri.clone())
                .header("Authorization", format!("token {}", env::var("GHP_TOKEN").unwrap()))
                .header("Accept", "application/vnd.github.raw")
                .header("X-Github-Api-Version", "2022-11-28")
                .header("User-Agent", "check-versions")
                .body(Body::empty())
                .unwrap();
            let client = client.clone();
            let result = tokio::spawn(async move {
                let res = client.request(request).await?;
                if res.status() == 404 {
                    println!("{:?}: {:?}", res.status(), uri.clone());
                }
                return body::to_bytes(res).await;
            });
            return result;
        })
        .buffered(PARALLEL_REQUESTS)
        .map_ok(|body| {
            let not_found = String::from("-------");

            let body_bytes = body.expect("error: no body");
            let body_str = match str::from_utf8(&body_bytes) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error converting body to UTF-8: {}", e);
                    return not_found.clone();
                }
            };

            let partial_package_lock_json: PartialPackageLockJson = match serde_json::from_str(body_str) {
                Ok(json) => json,
                Err(e) => {
                    eprintln!("Error parsing lockfile version: {}", e);
                    return not_found.clone();
                }
            };

            if let Some(lockfile_version) = partial_package_lock_json.lockfile_version {
                if lockfile_version == 1 {
                    let package_lock_json_v1: PackageLockJsonV1 = match serde_json::from_str(body_str) {
                        Ok(json) => json,
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            return not_found.clone();
                        }
                    };

                    if let Some(package) = package_lock_json_v1.dependencies.get(package_name) {
                        if let Some(version) = &package.version {
                            return version.clone();
                        }

                        return not_found.clone();
                    }

                    return not_found.clone();
                }
            }

            let package_lock_json: PackageLockJson = match serde_json::from_str(body_str) {
                Ok(json) => json,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    return not_found.clone();
                }
            };

            let node_modules_package_name = format!("node_modules/{}", package_name);
            if let Some(package) = package_lock_json.packages.get(&node_modules_package_name) {
                if let Some(version) = &package.version {
                    return version.clone();
                }

                return not_found.clone();
            }

            return not_found.clone();
        });

    let versions: Vec<_> = version_results.collect().await;
    for (i, version) in versions.iter().enumerate() {
        match version {
            Ok(version) => {
                let repos: Vec<&str> = json[i].split('/').collect();
                println!("{}\t: {}", version.as_str(), repos[1])
            },
            Err(e) => eprintln!("JoinError: {}", e),
        }
    }

    Ok(())
}
