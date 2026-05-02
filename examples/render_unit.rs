fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let user = args.first().map(String::as_str).unwrap_or("somfy-ci");
    let exec = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("/usr/local/bin/somfy --config /etc/somfy/config.toml serve");
    let gpio_chip = args.get(2).map(String::as_str).unwrap_or("/dev/gpiochip0");
    let spi_device = args.get(3).map(String::as_str).unwrap_or("/dev/spidev0.0");
    print!(
        "{}",
        somfy::commands::install::render_unit(user, exec, gpio_chip, spi_device)
    );
}
