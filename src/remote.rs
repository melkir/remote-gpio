use rppal::gpio::{Gpio, InputPin, OutputPin, Trigger};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

// Simplify Output enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Output {
    Select = 6,
    Down = 13,
    Stop = 19,
    Up = 26,
}

// Simplify Input enum
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Input {
    L1 = 21,
    L2 = 20,
    L3 = 16,
    L4 = 12,
    ALL,
}

impl std::fmt::Display for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Input::L1 => write!(f, "L1"),
            Input::L2 => write!(f, "L2"),
            Input::L3 => write!(f, "L3"),
            Input::L4 => write!(f, "L4"),
            Input::ALL => write!(f, "ALL"),
        }
    }
}

impl Input {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            21 => Some(Input::L1),
            20 => Some(Input::L2),
            16 => Some(Input::L3),
            12 => Some(Input::L4),
            _ => None,
        }
    }
}

// Simplify Command enum
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum Command {
    Select,
    Up,
    Stop,
    Down,
}

impl Command {
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "select" => Some(Command::Select),
            "up" => Some(Command::Up),
            "stop" => Some(Command::Stop),
            "down" => Some(Command::Down),
            _ => None,
        }
    }

    pub fn to_output(&self) -> Output {
        match self {
            Command::Select => Output::Select,
            Command::Up => Output::Up,
            Command::Stop => Output::Stop,
            Command::Down => Output::Down,
        }
    }
}

#[derive(Clone)]
pub struct RemoteControl {
    pub selection: Arc<Mutex<String>>,
    queue: Arc<Mutex<VecDeque<u8>>>,
    gpio: Arc<Gpio>,
    output_pins: Arc<Mutex<HashMap<Command, OutputPin>>>,
}

impl RemoteControl {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let gpio = Arc::new(Gpio::new()?);
        let mut output_pins = HashMap::new();

        // Initialize output pins
        for command in &[Command::Select, Command::Down, Command::Stop, Command::Up] {
            let output = command.to_output();
            let pin = gpio.get(output as u8)?.into_output();
            println!(
                "Inserting output pin: {:?} for command: {:?} on pin: {:?}",
                output as u8,
                command,
                pin.pin()
            );
            output_pins.insert(command.clone(), pin);
        }

        Ok(RemoteControl {
            selection: Arc::new(Mutex::new(String::new())),
            queue: Arc::new(Mutex::new(VecDeque::with_capacity(4))),
            gpio,
            output_pins: Arc::new(Mutex::new(output_pins)),
        })
    }

    pub fn observe(&self, led_pins: Vec<u8>) -> Result<mpsc::Receiver<String>, Box<dyn Error>> {
        let gpio = self.gpio.clone();
        let remote_control = self.clone();

        let (selection_tx, selection_rx) = mpsc::channel(100);

        for &pin_num in &led_pins {
            let mut pin = gpio.get(pin_num)?.into_input();
            pin.set_interrupt(Trigger::RisingEdge, Some(Duration::from_millis(70)))?;
            println!("Input pin set to interrupt: {}", pin_num);
        }

        tokio::spawn(async move {
            let mut pins = vec![];
            for &pin_num in &led_pins {
                println!("Getting input pin: {} to observe", pin_num);
                pins.push(gpio.get(pin_num).unwrap().into_input());
            }

            loop {
                let pin_refs: Vec<&InputPin> = pins.iter().collect();
                if let Ok(Some((pin, _level))) = gpio.poll_interrupts(&pin_refs, false, None) {
                    println!("Interrupt detected on pin: {}", pin.pin());
                    remote_control.on_event(&selection_tx, pin.pin());
                }
            }
        });

        Ok(selection_rx)
    }

    fn on_event(&self, selection_tx: &mpsc::Sender<String>, pin: u8) {
        let mut queue = self.queue.lock().unwrap();
        queue.push_back(pin);
        if queue.len() > 4 {
            queue.pop_front();
        }

        let next_selection = if queue.iter().collect::<HashSet<_>>().len() < 4 {
            Input::from_u8(*queue.back().unwrap()).unwrap().to_string()
        } else {
            Input::ALL.to_string()
        };

        println!("Next selection: {}", next_selection);

        let mut selection = self.selection.lock().unwrap();
        if selection.is_empty()
            || next_selection == "ALL"
            || matches!(
                (selection.as_str(), next_selection.as_str()),
                ("L1", "L2") | ("L2", "L3") | ("L3", "L4") | ("L4", "ALL") | ("ALL", "L1")
            )
        {
            *selection = next_selection.clone();
            drop(selection); // Release the lock before sending
            println!("Sending selection: {}", next_selection);
            let _ = selection_tx.try_send(next_selection);
        }
    }

    pub async fn send(&self, command: Command, led: Option<Input>) {
        let mut output_pins = self.output_pins.lock().unwrap();
        let pin: &mut OutputPin = output_pins.get_mut(&command).unwrap();

        println!("Sending command: {:?} for pin {:?}", command, pin.pin());

        if let Some(led) = led {
            let mut current_selection = self.selection.lock().unwrap().clone();
            while current_selection != led.to_string() {
                pin.set_low();
                std::thread::sleep(Duration::from_millis(500));
                pin.set_high();
                std::thread::sleep(Duration::from_millis(200));
                current_selection = self.selection.lock().unwrap().clone();
            }
        } else {
            pin.set_low();
            std::thread::sleep(Duration::from_millis(500));
            pin.set_high();
        }
    }
}
