import { init as ui } from "./ui.js";
import { init as library } from "./library.js";
import { init as player } from "./player.js";
import { init as queue } from "./queue.js";
import { init as sources } from "./sources.js";
import { init as menus } from "./menus.js";
import { init as random } from "./random.js";
import { init as connection } from "./connection.js";

ui();
library();
player();
queue();
sources();
menus();
random();
connection();
