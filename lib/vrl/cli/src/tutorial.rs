use super::{open_url, Error, Repl};
use chrono::DateTime;
use indoc::indoc;
use rustyline::{error::ReadlineError, Editor};
use vrl::{diagnostic::Formatter, state, Runtime, Target, Value};
use vrl_compiler::value;

struct Tutorial {
    section: usize,
    id: usize,
    title: &'static str,
    help_text: &'static str,
    // The URL endpoint (https://vrl.dev/:endpoint) for finding out more
    docs: &'static str,
    initial_event: Value,
    correct_answer: Value,
}

impl Tutorial {
    fn number(&self) -> String {
        format!("{}.{}", self.section, self.id)
    }
}

pub fn tutorial() -> Result<(), Error> {
    let mut index = 0;
    let mut compiler_state = state::Compiler::default();
    let mut rt = Runtime::new(state::Runtime::default());
    let mut rl = Editor::<Repl>::new();
    rl.set_helper(Some(Repl::new("> ")));

    let mut tutorials = tutorials();

    // Tutorial intro
    clear_screen();
    println!("{}", INTRO_TEXT);

    // Wait for "next" to continue
    {
        let mut rl = Editor::<Repl>::new();

        'intro: loop {
            match rl.readline("🚀 ").as_deref() {
                Ok(line) if line == "exit" || line == "quit" => {
                    println!("\nSee you next time! And don't forget to check out https://vrl.dev for more info!\n");
                    return Ok(());
                }
                Ok(line) if line == "start" => {
                    clear_screen();
                    break 'intro;
                }
                _ => {
                    println!("\nDidn't recognize that input. Type `next` and hit Enter to move on or `exit` to leave the VRL tutorial.\n");
                    continue;
                }
            }
        }
    }

    print_tutorial_help_text(0, &tutorials);

    'outer: loop {
        let readline = rl.readline("> ");
        match readline.as_deref() {
            Ok(line) if line == "exit" || line == "quit" => break 'outer,
            Ok(line) => {
                rl.add_history_entry(line);

                match line {
                    "" => continue,
                    "help" => help(),
                    "next" => {
                        clear_screen();

                        // End if no more tutorials are left, or else increment the index
                        if (index + 1) == tutorials.len() {
                            println!("\n\nCongratulations! You've successfully completed the VRL tutorial.\n");
                            break;
                        } else {
                            index = index.saturating_add(1);
                        }

                        print_tutorial_help_text(index, &tutorials);
                    }
                    "prev" => {
                        clear_screen();

                        if index == 0 {
                            println!("\n\nYou're back at the beginning!\n\n");
                        }

                        index = index.saturating_sub(1);
                        print_tutorial_help_text(index, &tutorials);
                    }
                    "docs" => {
                        let tut = &tutorials[index];
                        let endpoint = &tut.docs;
                        let docs_url = format!("https://vrl.dev/{}", endpoint);

                        open_url(&docs_url);

                        clear_screen();
                    }
                    command => {
                        let tut = &mut tutorials[index];
                        let event = &mut tut.initial_event;
                        let correct_answer = &tut.correct_answer;

                        // Purely for debugging
                        if command == "cheat" {
                            clear_screen();
                            println!("{}", correct_answer);
                        }

                        match resolve_to_value(event, &mut rt, command, &mut compiler_state) {
                            Ok(result) => {
                                if event == correct_answer {
                                    clear_screen();

                                    println!(
                                        "CORRECT! You've wisely ended up with this event:\n\n{}\n",
                                        event
                                    );

                                    // Exit if no more tutorials are left, otherwise move on to the next one
                                    if (index + 1) == tutorials.len() {
                                        println!("Congratulations! You've successfully completed the VRL tutorial.\n");
                                        break 'outer;
                                    } else {
                                        println!(
                                            "You've now completed tutorial {} out of {}.\nType `next` and hit Enter to move on to tutorial number {} or `exit` to leave the VRL tutorial.\n",
                                            index + 1,
                                            tutorials.len(),
                                            index + 2,
                                        );

                                        // Wait for "next" to continue
                                        {
                                            let mut rl = Editor::<Repl>::new();

                                            'next: loop {
                                                match rl.readline("🚀 ").as_deref() {
                                                    Ok(line)
                                                        if line == "exit" || line == "quit" =>
                                                    {
                                                        break 'outer
                                                    }
                                                    Ok(line) if line == "next" => {
                                                        clear_screen();
                                                        break 'next;
                                                    }
                                                    _ => {
                                                        println!("\nDidn't recognize that input. Type `next` and hit Enter to move on or `exit` to leave the VRL tutorial.\n");
                                                        continue;
                                                    }
                                                }
                                            }
                                        }

                                        index = index.saturating_add(1);
                                        print_tutorial_help_text(index, &tutorials);
                                    }
                                } else {
                                    println!("{}", result);
                                }
                            }
                            Err(err) => {
                                println!("{}", err);
                            }
                        }
                    }
                };
            }
            Err(ReadlineError::Interrupted) => break 'outer,
            Err(ReadlineError::Eof) => break 'outer,
            Err(err) => {
                println!("unable to read line: {}", err);
                break 'outer;
            }
        }
    }

    Ok(())
}

fn help() {
    println!("{}", HELP_TEXT);
}

fn print_tutorial_help_text(index: usize, tutorials: &[Tutorial]) {
    let tut = &tutorials[index];

    println!(
        "Tutorial {}: {}\n\n{}\nInitial event object:\n{}\n",
        tut.number(),
        tut.title,
        tut.help_text,
        tut.initial_event
    );
}

#[cfg(unix)]
fn clear_screen() {
    print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
}

#[cfg(windows)]
fn clear_screen() {
    print!("\x1b[2J");
}

// This function reworks the resolve function in repl.rs to return a Result rather than a String. If the Result is
// Ok, the value is used to check whether the current event is equal to the "correct" answer.
pub fn resolve_to_value(
    object: &mut dyn Target,
    runtime: &mut Runtime,
    program: &str,
    state: &mut state::Compiler,
) -> Result<Value, String> {
    let program = match vrl::compile_with_state(program, &stdlib::all(), state) {
        Ok(program) => program,
        Err(diagnostics) => {
            let msg = Formatter::new(program, diagnostics).colored().to_string();
            return Err(msg);
        }
    };

    match runtime.resolve(object, &program) {
        Ok(v) => Ok(v),
        Err(err) => Err(err.to_string()),
    }
}

// Help text
const HELP_TEXT: &str = r#"
Tutorial commands:
  .        Show the current value of the event
  docs     Open documentation for the current tutorial in your browser
  next     Load the next tutorial
  prev     Load the previous tutorial
  exit     Exit the VRL interactive tutorial shell
"#;

const INTRO_TEXT: &str = r#"Welcome to the Vector Remap Language (VRL)
interactive tutorial!

VRL is a language for working with observability data (logs and metrics) in
Vector. Here, you'll be guided through a series of tutorials that teach you how
to use VRL by solving problems. Tutorial commands:

  .        Show the current value of the event
  docs     Open documentation for the current tutorial in your browser
  next     Load the next tutorial
  prev     Load the previous tutorial
  exit     Exit the VRL interactive tutorial shell

Type `start` and hit Enter to begin.
"#;

fn tutorials() -> Vec<Tutorial> {
    let assignment_tut = Tutorial {
        section: 1,
        id: 1,
        title: "Assigning values to fields",
        docs: "expressions/#assignment",
        help_text: indoc! {r#"
            In VRL, you can assign values to fields like this:

            .field = "value"

            TASK:
            - Assign the string "hello" to the field `message`
        "#},
        initial_event: value![{}],
        correct_answer: value![{"message": "hello"}],
    };

    let deleting_fields_tut = Tutorial {
        section: 1,
        id: 2,
        title: "Deleting fields",
        docs: "functions/#del",
        help_text: indoc! {r#"
            You can delete fields from events using the `del` function:

            del(.field)

            TASK:
            - Delete fields `one` and `two` from the event
        "#},
        initial_event: value![{"one": 1, "two": 2, "three": 3}],
        correct_answer: value![{"three": 3}],
    };

    let exists_tut = Tutorial {
        section: 1,
        id: 3,
        title: "Existence checking",
        docs: "functions/#exists",
        help_text: indoc! {r#"
            You can check whether a field has a value using the `exists`
            function:

            exists(.field)

            TASK:
            - Make the event consist of just one `exists` field that indicates
              whether the `not_empty` field exists

            HINT:
            - You may need to use the `del` function too!
        "#},
        initial_event: value![{"not_empty": "This value does exist!"}],
        correct_answer: value![{"exists": true}],
    };

    let type_coercion_tut = Tutorial {
        section: 1,
        id: 4,
        title: "Type coercion",
        docs: "functions/#coerce-functions",
        help_text: indoc! {r#"
            You can coerce VRL values into other types using the `to_*` coercion
            functions (`to_bool`, `to_string`, etc.).

            TASK:
            - Coerce all of the fields in this event into the type suggested by
              the key (i.e. convert key `boolean` into a Boolean and so on)
        "#},
        initial_event: value![{"boolean": "yes", "integer": "1337", "float": "42.5", "string": true}],
        correct_answer: value![{"boolean": true, "integer": 1337, "float": 42.5, "string": "true"}],
    };

    let parse_json_tut = Tutorial {
        section: 2,
        id: 1,
        title: "Parsing JSON",
        docs: "functions/#parse_json",
        help_text: indoc! {r#"
            You can parse inputs to JSON in VRL using the `parse_json` function:

            parse_json(.field)

            `parse_json` is fallible, so make sure to handle potential errors!

            TASK:
            - Set the value of the event to the `message` field parsed as JSON
        "#},
        initial_event: value![{"message": r#"{"severity":"info","message":"Coast is clear"}"#, "timestamp": "2021-02-16T00:25:12.728003Z"}],
        correct_answer: value![{"severity": "info", "message": "Coast is clear"}],
    };

    let example_timestamp = Value::Timestamp(
        DateTime::parse_from_rfc3339("2020-12-19T21:48:09.004Z")
            .unwrap()
            .into(),
    );

    let parse_syslog_tut = Tutorial {
        section: 2,
        id: 2,
        title: "Parsing Syslog",
        docs: "functions/#parse_syslog",
        help_text: indoc! {r#"
            You can parse Syslog messages into named fields using the `parse_syslog` function:

            parse_syslog(.field)

            `parse_syslog` is fallible, so make sure to handle potential errors!

            TASK:
            - Set the value of the event to the `message` field parsed from Syslog
        "#},
        initial_event: value![{"message": "<12>3 2020-12-19T21:48:09.004Z initech.io su 4015 ID81 - TPS report missing cover sheet", "timestamp": "2020-12-19T21:48:09.004Z"}],
        correct_answer: value![{"appname": "su", "facility": "user", "hostname": "initech.io", "message": "TPS report missing cover sheet", "msgid": "ID81", "procid": 4015, "severity": "warning", "timestamp": example_timestamp}],
    };

    let parse_kv_tut = Tutorial {
        section: 2,
        id: 3,
        title: "Parsing key-value logs",
        docs: "functions/#parse_key_value",
        help_text: indoc! {r#"

        "#},
        initial_event: value![{"message": r#"@timestamp="2020-12-19T21:48:09.004Z" severity=info msg="Smooth sailing over here""#}],
        correct_answer: value![{"@timestamp": "2020-12-19T21:48:09.004Z", "msg": "Smooth sailing over here", "severity": "info"}],
    };

    vec![
        assignment_tut,
        deleting_fields_tut,
        exists_tut,
        type_coercion_tut,
        parse_json_tut,
        parse_syslog_tut,
        parse_kv_tut,
    ]
}
