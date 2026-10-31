fn main() {
    crossbar::util::set_prog("cbar");
    crossbar::cli::main(std::env::args().skip(1).collect());
}
