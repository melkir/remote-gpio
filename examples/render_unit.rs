fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let user = args.first().map(String::as_str).unwrap_or("somfy-ci");
    let exec = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("/usr/local/bin/somfy");
    print!("{}", somfy::commands::install::render_unit(user, exec));
}
