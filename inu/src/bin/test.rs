use std::collections::BTreeSet;

enum Error {
    One,
    Two,
    Three,
}

enum Results {
    Sucess,
    Failed(Error),
}

fn main() {
    let res = Results::Failed(Error::Two);

    match res {
        Results::Failed(Error::One) => {
            println!("Error One");
        }
        Results::Failed(Error::Two) => {
            println!("Error Two");
        }
        Results::Failed(Error::Three) => {
            println!("Error Three");
        }
        Results::Sucess => {
            println!("Success");
        }
    }
}
