use std::io;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode};
use crossterm::{event, terminal};
use ratatui::prelude::CrosstermBackend;
use ratatui::text::Text;
use ratatui::{Frame, Terminal};
use tears::prelude::*;
use tears::subscription::terminal::TerminalEvents;
use tears::subscription::time::{Message as TimerMessage, Timer};

#[derive(Debug, Clone)]
enum Message {
    Timer(TimerMessage),
    Terminal(crossterm::event::Event),
}

#[derive(Debug, Clone, Default)]
struct Counter {
    count: u32,
}

impl Application for Counter {
    type Message = Message;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self::default(), Command::none())
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            Message::Timer(TimerMessage::Tick) => {
                self.count += 1;

                Command::none()
            }
            Message::Terminal(event) => {
                // Handle keyboard events
                if let Event::Key(key) = event {
                    if key.code == KeyCode::Char('q') {
                        return Command::effect(Action::Quit);
                    }
                }

                Command::none()
            }
        }
    }

    fn view(&self, frame: &mut Frame<'_>) {
        let widget = Text::raw(self.count.to_string());
        frame.render_widget(widget, frame.area());
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![
            Subscription::new(Timer::new(1000)).map(Message::Timer),
            Subscription::new(TerminalEvents::new()).map(Message::Terminal),
        ]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let runtime = Runtime::<Counter>::new(());

    // Setup terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = runtime.run(&mut terminal, 16).await;

    // Restore terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        terminal::LeaveAlternateScreen,
        event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}
