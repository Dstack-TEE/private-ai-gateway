use clap::Command;
use serde_json::{json, Value};

pub(super) fn command(command: &Command) -> Value {
    json!({
        "name": command.get_name(),
        "version": command.get_version(),
        "about": command.get_about().map(ToString::to_string),
        "longAbout": command.get_long_about().map(ToString::to_string),
        "arguments": command.get_arguments().map(argument).collect::<Vec<_>>(),
        "commands": command.get_subcommands().map(self::command).collect::<Vec<_>>(),
    })
}

fn argument(argument: &clap::Arg) -> Value {
    let takes_value = argument.get_action().takes_values();
    let possible_values = if takes_value {
        argument
            .get_possible_values()
            .into_iter()
            .filter(|value| !value.is_hide_set())
            .map(|value| value.get_name().to_owned())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    json!({
        "id": argument.get_id().as_str(),
        "long": argument.get_long(),
        "short": argument.get_short().map(|short| short.to_string()),
        "index": argument.get_index(),
        "visibleAliases": argument.get_visible_aliases(),
        "visibleShortAliases": argument.get_visible_short_aliases(),
        "help": argument.get_help().map(ToString::to_string),
        "longHelp": argument.get_long_help().map(ToString::to_string),
        "required": argument.is_required_set(),
        "global": argument.is_global_set(),
        "takesValue": takes_value,
        "numArgs": argument.get_num_args().map(|range| json!({
            "min": range.min_values(),
            "max": range.max_values(),
        })),
        "valueNames": argument.get_value_names().map(|names| {
            names.iter().map(ToString::to_string).collect::<Vec<_>>()
        }),
        "defaultValues": argument.get_default_values().iter().map(|value| {
            value.to_string_lossy().into_owned()
        }).collect::<Vec<_>>(),
        "possibleValues": possible_values,
    })
}
