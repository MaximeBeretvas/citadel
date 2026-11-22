fn main() -> anyhow::Result<()> {
    citadel::init_tracing();
    citadel::cli::run_from_args()
}
