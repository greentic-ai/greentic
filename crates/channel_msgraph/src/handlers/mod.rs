use dashmap::DashMap;

pub mod calendar;
pub mod email;
pub mod onedrive;
pub mod teams;
pub mod sharepoint;


pub fn init_all() {
    email::init();
    calendar::init();
    teams::init();
    onedrive::init();
    sharepoint::init();
}

pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let mut all = vec![];
    all.extend(calendar::get_client_states());
    all.extend(email::get_client_states());
    all.extend(teams::get_client_states(config));
    all.extend(onedrive::get_client_states(config));
    all.extend(sharepoint::get_client_states(config));
    all
}