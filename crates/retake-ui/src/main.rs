#![windows_subsystem = "windows"]
fn view(_: &u32) -> iced::Element<'_, ()> {
    iced::widget::text("Retake").into()
}
fn main() -> iced::Result {
    iced::run("Retake", |_: &mut u32, _: ()| {}, view)
}
