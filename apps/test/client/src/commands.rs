// Duplicated for simplicity with the corresponding module in the app crate.
// Make sure to keep them in sync.

#[derive(Debug)]
pub enum Command {
    Reverse,
    AddNumbers,
    Base58Encode,
    Sha256,
    CountPrimes,
    ShowUxScreen = 0x80,
    DeviceProp = 0x81,
    Draw = 0x82,
    // 0x83 was the retired kolibri-embedded-gui demo.
    GuiAccel = 0x84,
    SceneGui = 0x85,
    // Runs the display-ABI contract self-test (sdk::abi_probe); returns a u64 BE
    // bitmask of failed checks (0 = the contract holds).
    DisplayCodes = 0x86,
    Print = 0xfd,
    Panic = 0xfe,
    Exit = 0xff,
}

impl TryFrom<u8> for Command {
    type Error = ();

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        match byte {
            0x00 => Ok(Command::Reverse),
            0x01 => Ok(Command::AddNumbers),
            0x02 => Ok(Command::Base58Encode),
            0x03 => Ok(Command::Sha256),
            0x04 => Ok(Command::CountPrimes),
            0x80 => Ok(Command::ShowUxScreen),
            0x81 => Ok(Command::DeviceProp),
            0x82 => Ok(Command::Draw),
            0x84 => Ok(Command::GuiAccel),
            0x85 => Ok(Command::SceneGui),
            0x86 => Ok(Command::DisplayCodes),
            0xfd => Ok(Command::Print),
            0xfe => Ok(Command::Panic),
            0xff => Ok(Command::Exit),
            _ => Err(()),
        }
    }
}
