#![cfg_attr(target_os = "windows", allow(linker_messages))]

mod actions;
#[cfg(feature = "cli")]
pub mod admin;
#[cfg(feature = "cli")]
mod admin_daemon;
#[cfg(feature = "cli")]
mod admin_security;
#[cfg(feature = "cli")]
mod admin_service;
#[cfg(feature = "desktop")]
mod app_state;
mod async_runtime;
mod auth;
mod brand;
pub mod build_identity;
mod canvs;
mod canvs_web;
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "desktop")]
mod commands;
pub mod control;
pub mod daemon;
mod data;
mod error;
pub mod gateway_control;
pub mod gateway_daemon;
pub mod harness;
mod health;
#[cfg(feature = "desktop")]
mod legacy_desktop;
mod logging;
mod management;
mod mcp;
mod platform;
pub mod rollout;
mod runtime;
mod secret;
mod settings;
mod skills;
pub mod tools;
mod tunnel;
#[cfg(target_os = "windows")]
pub mod windows_service;
mod workspace;

#[cfg(feature = "desktop")]
pub use legacy_desktop::run;
