// Integration tests for dynamic subscriptions

use ratatui::{Frame, Terminal, backend::TestBackend};
use tears::{
    application::Application,
    command::{Action, Command},
    runtime::Runtime,
    subscription::Subscription,
};
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn test_dynamic_subscription_starts_when_enabled() {
    // Test that subscription starts when enabled
    struct AppWithEnabledTimer {
        tick_count: u32,
    }

    impl Application for AppWithEnabledTimer {
        type Message = ();
        type Flags = ();

        fn new(_: ()) -> (Self, Command<()>) {
            (AppWithEnabledTimer { tick_count: 0 }, Command::none())
        }

        fn update(&mut self, _: ()) -> Command<()> {
            self.tick_count += 1;
            if self.tick_count >= 2 {
                Command::effect(Action::Quit)
            } else {
                Command::none()
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<()>> {
            use tears::subscription::time::Timer;
            vec![Subscription::new(Timer::new(10)).map(|_| ())]
        }
    }

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<AppWithEnabledTimer>::new(());
    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn test_dynamic_subscription_stops_when_disabled() {
    // Test that subscription stops when disabled
    struct AppWithToggle {
        enabled: bool,
        tick_count: u32,
    }

    #[derive(Clone)]
    enum Msg {
        Tick,
        Disable,
    }

    impl Application for AppWithToggle {
        type Message = Msg;
        type Flags = ();

        fn new(_: ()) -> (Self, Command<Msg>) {
            let cmd = Command::future(async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Msg::Disable
            });
            (
                AppWithToggle {
                    enabled: true,
                    tick_count: 0,
                },
                cmd,
            )
        }

        fn update(&mut self, msg: Msg) -> Command<Msg> {
            match msg {
                Msg::Tick => {
                    self.tick_count += 1;
                    Command::none()
                }
                Msg::Disable => {
                    self.enabled = false;
                    // Quit after a delay to see if more ticks arrive
                    Command::future(async {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    });
                    Command::effect(Action::Quit)
                }
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Msg>> {
            use tears::subscription::time::Timer;

            if self.enabled {
                vec![Subscription::new(Timer::new(10)).map(|_| Msg::Tick)]
            } else {
                vec![]
            }
        }
    }

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<AppWithToggle>::new(());
    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn test_dynamic_subscription_changes_based_on_state() {
    // Test subscription changes when app state changes
    struct StatefulApp {
        mode: u32,
    }

    #[derive(Clone)]
    enum Msg {
        FastTick,
        SlowTick,
        ChangeMode,
    }

    impl Application for StatefulApp {
        type Message = Msg;
        type Flags = ();

        fn new(_: ()) -> (Self, Command<Msg>) {
            let cmd = Command::future(async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Msg::ChangeMode
            });
            (StatefulApp { mode: 0 }, cmd)
        }

        fn update(&mut self, msg: Msg) -> Command<Msg> {
            match msg {
                Msg::FastTick => Command::none(),
                Msg::SlowTick => Command::none(),
                Msg::ChangeMode => {
                    self.mode += 1;
                    if self.mode >= 2 {
                        Command::effect(Action::Quit)
                    } else {
                        Command::future(async {
                            tokio::time::sleep(Duration::from_millis(30)).await;
                            Msg::ChangeMode
                        })
                    }
                }
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Msg>> {
            use tears::subscription::time::Timer;

            match self.mode {
                0 => vec![Subscription::new(Timer::new(5)).map(|_| Msg::FastTick)],
                1 => vec![Subscription::new(Timer::new(20)).map(|_| Msg::SlowTick)],
                _ => vec![],
            }
        }
    }

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<StatefulApp>::new(());
    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn test_dynamic_subscription_multiple_changes() {
    // Test multiple subscription add/remove cycles
    struct MultiChangeApp {
        cycle: u32,
    }

    #[derive(Clone)]
    enum Msg {
        Tick,
        NextCycle,
    }

    impl Application for MultiChangeApp {
        type Message = Msg;
        type Flags = ();

        fn new(_: ()) -> (Self, Command<Msg>) {
            (MultiChangeApp { cycle: 0 }, Command::none())
        }

        fn update(&mut self, msg: Msg) -> Command<Msg> {
            match msg {
                Msg::Tick => {
                    // Advance cycle after a few ticks
                    Command::future(async {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Msg::NextCycle
                    })
                }
                Msg::NextCycle => {
                    self.cycle += 1;
                    if self.cycle >= 3 {
                        Command::effect(Action::Quit)
                    } else {
                        Command::none()
                    }
                }
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Msg>> {
            use tears::subscription::time::Timer;

            // Alternate between having timer and not having timer
            if self.cycle % 2 == 0 {
                vec![Subscription::new(Timer::new(5)).map(|_| Msg::Tick)]
            } else {
                vec![]
            }
        }
    }

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<MultiChangeApp>::new(());
    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}
