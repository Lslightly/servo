import AbilityConstant from '@ohos.app.ability.AbilityConstant';
import hilog from '@ohos.hilog';
import UIAbility from '@ohos.app.ability.UIAbility';
import Want from '@ohos.app.ability.Want';
import window from '@ohos.window';
import servoshell, { InitOpts } from 'libservoshell.so';

const WRONG_COMMAND_ARRAY = ["tracing", "devtools", "force_ipc", "multiprocess", "webdriver"];

// `--chrome=none` hides the app's own toolbar and tab bar so the XComponent
// fills the window. Measurement runs want the web content alone: the chrome
// costs ArkUI layout on every frame and, more importantly, shrinks the
// viewport, which changes how much the page has to lay out.
//
// It is handled here and *not* forwarded to servoshell, whose argument parser
// exits the process on an option it does not recognise. The same spelling
// works for the ArkWeb test app, so a measurement harness can pass one flag to
// both.
//
// `--psn=<key>` — the form `aa start` needs for a single argv token — creates a
// want parameter whose *name* is everything after the `=` and whose value is
// null, so `--psn=--chrome=none` arrives as the key `--chrome=none`. `--ps
// chrome none` instead produces a real key/value pair. Accept both.
function isChromeKey(key: string): boolean {
  return key === "--chrome" || key === "chrome";
}

function disablesChrome(value: string): boolean {
  return value === "none" || value === "false" || value === "0";
}

export default class EntryAbility extends UIAbility {
  init_params: LocalStorage = new LocalStorage();
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam) {
    if (servoshell === undefined) {
      hilog.fatal(0xE0C3, "Servo EntryAbility", "Failed to load libservoshell.so! Search hilog for the reason.");
      throw new Error("Failed to load libservoshell.so");
    }
    let uri: string = want.uri || "https://servo.org"
    let params: string[] = []
    let showChrome: boolean = true
    if (typeof want.parameters !== "undefined") {
      Object.entries(want.parameters).forEach((entry) => {
        let key = entry[0]
        // The commandline parameters we are interested in all start with `--`.
        // OpenHarmony also adds quite a few default parameters to the wants.parameters
        // dictionary, and servo currently fails with an error if it receives unknown
        // options.
        // On the commandline you can pass parameters via the `aa` tool.
        // ```
        // aa start -a EntryAbility -b org.servo.servoshell --ps=--screen-size 505x413
        // ```
        // Use `--ps=--arg_name value` for key-value parameters. Please note that
        // `--ps` and `--<arg_name>` must be connected by an `=` and may not be seperated.
        // the value must be space seperated, since `aa start` will expect it as a second parameter.
        // Use `--psn=--arg_name` for flags that do not have a value.
        // See the aa tool documentation for more details:
        // https://docs.openharmony.cn/pages/v5.0/en/application-dev/tools/aa-tool.md
        let raw_value: string = entry[1] !== null && entry[1] !== undefined ? entry[1].toString() : ""
        // Split `--key=value` keys produced by `--psn=`, so both spellings
        // reach the checks below as a key and a value.
        let split_key = key
        let split_value = raw_value
        let eq = key.indexOf("=")
        if (raw_value.length === 0 && eq > 0) {
          split_key = key.substring(0, eq)
          split_value = key.substring(eq + 1)
        }
        if (isChromeKey(split_key)) {
          showChrome = !disablesChrome(split_value)
        } else if (key.startsWith("--")) {
          params.push(key)
          if (raw_value.length > 0) {
            params.push(raw_value)
          }
        } else if (WRONG_COMMAND_ARRAY.some((prefix: string) => key.startsWith(prefix))) {
          hilog.error(0xE0C3, "Servo EntryAbility", "-------------------------------------------------------------------------------------------------");
          hilog.error(0xE0C3, "Servo EntryAbility", "\n\nYou probably meant to add -- infront of your argument, i.e., --psn=--tracing vs --psn=tracing. You used " + entry);
          hilog.error(0xE0C3, "Servo EntryAbility", "-------------------------------------------------------------------------------------------------");
          return;
        }
      })
    }
    let servoshell_params = params.join("\u{001f}")
    hilog.debug(0x0000, 'Servo EntryAbility', 'Servoshell parameters: %{public}s', servoshell_params);
    this.init_params.setOrCreate<boolean>('showChrome', showChrome)
    hilog.info(0x0000, 'Servo EntryAbility', 'showChrome: %{public}s', `${showChrome}`);

    let resource_dir: string = this.context.resourceDir;
    console.debug("resourceDir: ", resource_dir);
    let init_options: InitOpts = {
      url: uri,
      resourceDir: resource_dir,
      commandlineArgs: servoshell_params,
    }
    servoshell.initServo(init_options)
  }

  onDestroy() {
    hilog.info(0x0000, 'testTag', '%{public}s', 'Ability onDestroy');
  }

  onWindowStageCreate(windowStage: window.WindowStage) {
    // Main window is created, set main page for this ability
    hilog.info(0x0000, 'testTag', '%{public}s', 'Ability onWindowStageCreate');

    windowStage.loadContent('pages/Index', this.init_params, (err, data) => {
      if (err.code) {
        hilog.error(0x0000, 'testTag', 'Failed to load the content. Cause: %{public}s', JSON.stringify(err) ?? '');
        return;
      }
      hilog.info(0x0000, 'testTag', 'Succeeded in loading the content. Data: %{public}s', JSON.stringify(data) ?? '');
    });
  }

  onWindowStageDestroy() {
    // Main window is destroyed, release UI related resources
    hilog.info(0x0000, 'testTag', '%{public}s', 'Ability onWindowStageDestroy');
  }

  onForeground() {
    // Ability has brought to foreground
    hilog.info(0x0000, 'testTag', '%{public}s', 'Ability onForeground');
  }

  onBackground() {
    // Ability has back to background
    hilog.info(0x0000, 'testTag', '%{public}s', 'Ability onBackground');
  }
};
