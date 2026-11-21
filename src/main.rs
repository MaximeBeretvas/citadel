fn main() -> anyhow::Result<()> {
    web_vault::init_tracing();
    web_vault::cli::run_from_args()
}
