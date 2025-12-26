use std::io;

use color_eyre::eyre::Result;
use crossterm::{event, terminal};
use ratatui::prelude::CrosstermBackend;
use ratatui::text::Text;
use ratatui::{Frame, Terminal};
use tears::application::Application;
use tears::command::Command;
use tears::runtime::Runtime;
use tears::subscription::time::TimeSub;

#[derive(Debug, Clone)]
enum Message {
    Tick,
}

#[derive(Debug, Clone, Default)]
struct Timer {
    count: u32,
}

impl Application<TimeSub<Message>> for Timer {
    type Message = Message;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self::default(), Command::none())
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            Message::Tick => {
                self.count += 1;

                Command::none()
            }
        }
    }

    fn view(&self, frame: &mut Frame<'_>) {
        let widget = Text::raw(self.count.to_string());
        frame.render_widget(widget, frame.area());
    }

    fn subscriptions(&self) -> Vec<TimeSub<Message>> {
        vec![TimeSub::new(1000, || Message::Tick)]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let (app, _cmd) = Timer::new(());
    let runtime = Runtime::new(app);

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
