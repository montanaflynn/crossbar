fn main() {
    crossbar::util::set_prog("switchboard");
    crossbar::board::main(std::env::args().skip(1).collect());
}
