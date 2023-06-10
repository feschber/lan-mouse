use std::io::{self, Write};

use crate::client::Position;


pub fn ask_confirmation(default: bool) -> Result<bool, io::Error> {
    eprint!("{}", if default {" [Y,n] "} else { " [y,N] "});
    io::stderr().flush()?;
    let answer = loop {
        let mut buffer = String::new();
        io::stdin().read_line(&mut buffer)?;
        let answer = buffer.to_lowercase();
        let answer = answer.trim();
        match answer {
            "" | "y" => break true,
            "n" => break false,
            _ => {
                eprint!("Enter y for Yes or n for No: ");
                io::stderr().flush()?;
                continue
            }
        }
    };
    Ok(answer)
}

pub fn ask_position() -> Result<Position, io::Error> {
    let pos = loop {
        let mut buffer = String::new();
        io::stdin().read_line(&mut buffer)?;
        let answer = buffer.to_lowercase();
        let answer = answer.trim();
        match answer {
            "t" | "top" => break Position::Top,
            "b" | "bottom" => break Position::Bottom,
            "l" | "left" => break Position::Right,
            "r" | "right" => break Position::Left,
            _ => {
                eprint!("Enter top/t bottom/b left/l or right/r: ");
                io::stderr().flush()?;
                continue
            }
        };
    };
    Ok(pos)
}
