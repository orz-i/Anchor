fn main() {
    eprintln!(
        "warning: `coding-tools-mcp` has been renamed to `anchor`; this compatibility alias will be removed in a future release"
    );
    std::process::exit(anchor_lib::cli::run());
}
