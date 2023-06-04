use honggfuzz::fuzz;
use pgcat::messages::simple_query;

fn main() {
    // Define the fuzzing loop
    loop {
        // Fuzzing input will be provided by Honggfuzz
        fuzz!(|data: &[u8]| {
            // Convert the input to a string
            if let Ok(query) = std::str::from_utf8(data) {
                let _ = simple_query(query);
                

            }
        });
    }
}
