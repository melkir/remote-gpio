fn main() {
    print!(
        "{}",
        somfy::commands::install::render_unit("somfy-ci", "/usr/local/bin/somfy")
    );
}
