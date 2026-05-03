use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div};

/// The main dashboard panel rendered inside the `Shell` layout.
pub struct DashboardHome;

impl DashboardHome {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for DashboardHome {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("Dashboard content")
    }
}
