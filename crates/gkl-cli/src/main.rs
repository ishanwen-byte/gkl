//! gkl: git 历史考古 CLI。

mod app;

fn main() -> anyhow::Result<()> {
    app::run()
}
