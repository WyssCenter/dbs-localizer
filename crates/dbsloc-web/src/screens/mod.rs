// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The screens of `docs/SERVER_CLIENT_PLAN.md` §5, in the order a user meets them.
//!
//! Each screen is a plain function over the shared [`WebApp`](crate::app::WebApp) state and an
//! `egui::Ui`; the server data driving it comes from `dbsloc_api` DTOs.

pub(crate) mod guided;
pub(crate) mod landing;
pub(crate) mod progress;
pub(crate) mod qc;
pub(crate) mod review;
pub(crate) mod signin;
pub(crate) mod upload;
