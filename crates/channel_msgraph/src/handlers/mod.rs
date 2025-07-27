use std::sync::Arc;

use dashmap::DashMap;

pub mod calendar;
pub mod email;
pub mod onedrive;
pub mod teams;
pub mod sharepoint;


pub fn init_all(config: Arc<DashMap<String, String>>) {
    email::init(config.clone());
    calendar::init(config.clone());
    teams::init(config.clone());
    onedrive::init(config.clone());
    sharepoint::init(config.clone());
}

pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let mut all = vec![];
    all.extend(calendar::get_client_states(config));
    all.extend(email::get_client_states(config));
    all.extend(teams::get_client_states(config));
    all.extend(onedrive::get_client_states(config));
    all.extend(sharepoint::get_client_states(config));
    all
}