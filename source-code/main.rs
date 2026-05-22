// SPDX-License-Identifier: Apache-2.0
// Copyright 2025 Hammer Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0

mod cache;
mod cli;
mod db;
mod deb;
mod diff;
mod download;
mod gpg;
mod grub;
mod hk_tools;
mod livecheck;
mod livepatch;
mod log;
mod package;
mod profile;
mod repo;
mod selfupdate;
mod setup;
mod solver;
mod store;
mod transaction;
mod ui;
mod userenv;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    crate::log::session_start();
    if let Err(e) = cli::run(args).await {
        crate::ui::fatal(&format!("{:#}", e));
        std::process::exit(1);
    }
}
