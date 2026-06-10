//! The fight action log — a bit-exact port of the reference
//! `action/Actions.java` and all `action/Action*.java` entries.
//!
//! Every `Action` variant encodes to the same compact JSON array the official
//! Java generator emits: `[action_type_id, field…]`.  See `to_json` on each
//! variant (and the Java class quoted in the doc comment) for the exact wire
//! format.
//!
//! # logID / effect-id mechanics
//! `ActionAddEffect::create_effect` mirrors the Java static factory that calls
//! `Actions.getEffectId()` (an auto-increment counter) and returns the assigned
//! id.  `ActionStackEffect`, `ActionUpdateEffect`, and `ActionRemoveEffect` all
//! reference that id, so callers must preserve the `u32` returned by
//! `create_effect` and pass it to those constructors.
//!
//! # ActionLog
//! `ActionLog` mirrors `Actions.java`.  It owns the action list and the
//! `next_effect_id` counter.  `to_json()` returns only the `actions` array
//! (the `leeks`/`map`/`dead`/`ops` top-level fields are assembled by the
//! orchestrating layer that also holds entity descriptors and statistics).

use std::collections::HashMap;

use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Action-type id constants (mirrors Action.java interface constants)
// ---------------------------------------------------------------------------

/// `Action.START_FIGHT = 0`
pub const START_FIGHT: i64 = 0;
/// `Action.END_FIGHT = 4`
pub const END_FIGHT: i64 = 4;
/// `Action.PLAYER_DEAD = 5`
pub const PLAYER_DEAD: i64 = 5;
/// `Action.NEW_TURN = 6`
pub const NEW_TURN: i64 = 6;
/// `Action.LEEK_TURN = 7`
pub const LEEK_TURN: i64 = 7;
/// `Action.END_TURN = 8`
pub const END_TURN: i64 = 8;
/// `Action.SUMMON = 9`
pub const SUMMON: i64 = 9;
/// `Action.MOVE_TO = 10`
pub const MOVE_TO: i64 = 10;
/// `Action.KILL = 11`
pub const KILL: i64 = 11;
/// `Action.USE_CHIP = 12`
pub const USE_CHIP: i64 = 12;
/// `Action.SET_WEAPON = 13`
pub const SET_WEAPON: i64 = 13;
/// `Action.STACK_EFFECT = 14`
pub const STACK_EFFECT: i64 = 14;
/// `Action.CHEST_OPENED = 15`
pub const CHEST_OPENED: i64 = 15;
/// `Action.USE_WEAPON = 16`
pub const USE_WEAPON: i64 = 16;

// Buff / damage type ids
/// `Action.LOST_PT = 100`  (not a DamageType but used in damage context as a stat-drain)
pub const LOST_PT: i64 = 100;
/// `Action.LOST_LIFE = 101` — direct damage (`DamageType.DIRECT.value`)
pub const LOST_LIFE: i64 = 101;
/// `Action.LOST_PM = 102`
pub const LOST_PM: i64 = 102;
/// `Action.HEAL = 103`
pub const HEAL: i64 = 103;
/// `Action.VITALITY = 104`
pub const VITALITY: i64 = 104;
/// `Action.RESURRECT = 105`
pub const RESURRECT: i64 = 105;
/// `Action.LOSE_STRENGTH = 106`
pub const LOSE_STRENGTH: i64 = 106;
/// `Action.NOVA_DAMAGE = 107` — nova damage (`DamageType.NOVA.value`)
pub const NOVA_DAMAGE: i64 = 107;
/// `Action.DAMAGE_RETURN = 108` — damage return (`DamageType.RETURN.value`)
pub const DAMAGE_RETURN: i64 = 108;
/// `Action.LIFE_DAMAGE = 109` — life damage (`DamageType.LIFE.value`)
pub const LIFE_DAMAGE: i64 = 109;
/// `Action.POISON_DAMAGE = 110` — poison / aftereffect (`DamageType.POISON.value`)
pub const POISON_DAMAGE: i64 = 110;
/// `Action.AFTEREFFECT = 111`
pub const AFTEREFFECT: i64 = 111;
/// `Action.NOVA_VITALITY = 112`
pub const NOVA_VITALITY: i64 = 112;

// "Fun" action ids
/// `Action.LAMA = 201`
pub const LAMA: i64 = 201;
/// `Action.SAY = 203`
pub const SAY: i64 = 203;
/// `Action.SHOW_CELL = 205`
pub const SHOW_CELL: i64 = 205;

// Effect action ids
/// `Action.ADD_WEAPON_EFFECT = 301`
pub const ADD_WEAPON_EFFECT: i64 = 301;
/// `Action.ADD_CHIP_EFFECT = 302`
pub const ADD_CHIP_EFFECT: i64 = 302;
/// `Action.REMOVE_EFFECT = 303`
pub const REMOVE_EFFECT: i64 = 303;
/// `Action.UPDATE_EFFECT = 304`
pub const UPDATE_EFFECT: i64 = 304;
/// `Action.REDUCE_EFFECTS = 306`
pub const REDUCE_EFFECTS: i64 = 306;
/// `Action.REMOVE_POISONS = 307`
pub const REMOVE_POISONS: i64 = 307;
/// `Action.REMOVE_SHACKLES = 308`
pub const REMOVE_SHACKLES: i64 = 308;

// Misc
/// `Action.ERROR = 1000`
pub const ERROR: i64 = 1000;
/// `Action.MAP = 1001`
pub const MAP: i64 = 1001;
/// `Action.AI_ERROR = 1002`
pub const AI_ERROR: i64 = 1002;

// ---------------------------------------------------------------------------
// DamageType mirror (DamageType.java)
// ---------------------------------------------------------------------------

/// Wire value for each damage category. Maps to the action-type id emitted as
/// the first element of the action array (e.g. `DamageType::Direct` → 101).
///
/// Java: `enum DamageType { DIRECT(101), NOVA(107), RETURN(108), LIFE(109),
///        POISON(110), AFTEREFFECT(110) }`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageType {
    /// `DamageType.DIRECT` → 101
    Direct,
    /// `DamageType.NOVA` → 107
    Nova,
    /// `DamageType.RETURN` → 108
    Return,
    /// `DamageType.LIFE` → 109
    Life,
    /// `DamageType.POISON` / `DamageType.AFTEREFFECT` → 110
    Poison,
}

impl DamageType {
    /// The integer emitted into the JSON array (mirrors `DamageType.value`).
    #[must_use]
    pub const fn wire_value(self) -> i64 {
        match self {
            Self::Direct => 101,
            Self::Nova => 107,
            Self::Return => 108,
            Self::Life => 109,
            Self::Poison => 110,
        }
    }
}

// ---------------------------------------------------------------------------
// Attack type constants — used by ActionAddEffect to decide ADD_WEAPON_EFFECT
// vs ADD_CHIP_EFFECT.  Mirrors Attack.TYPE_WEAPON / TYPE_CHIP in Attack.java.
// ---------------------------------------------------------------------------

/// `Attack.TYPE_WEAPON = 1`
pub const ATTACK_TYPE_WEAPON: i64 = 1;
/// `Attack.TYPE_CHIP = 2`
pub const ATTACK_TYPE_CHIP: i64 = 2;

// ---------------------------------------------------------------------------
// The Action enum
// ---------------------------------------------------------------------------

/// One logged fight event.  Each variant corresponds to one `Action*.java`
/// class.  `to_json()` produces the exact compact array the Java `getJSON()`
/// method emits.
#[derive(Debug, Clone)]
pub enum Action {
    // -----------------------------------------------------------------------
    // Flow / turn control
    // -----------------------------------------------------------------------
    /// `ActionStartFight` — `[0]`
    ///
    /// Java: `retour.add(Action.START_FIGHT)` (team1/team2 stored but not emitted)
    StartFight { team1: i32, team2: i32 },

    /// `ActionNewTurn` — `[6, count]`
    ///
    /// Java: `retour.add(Action.NEW_TURN); retour.add(count)`
    NewTurn { count: i32 },

    /// `ActionEntityTurn` — `[7, entity_id]`
    ///
    /// Java: `retour.add(Action.LEEK_TURN); retour.add(id)` (id = -1 if leek == null)
    EntityTurn { entity_id: i64 },

    /// `ActionEndTurn` — `[8, entity_id, tp_left, mp_left]`
    ///
    /// Java: `json.add(Action.END_TURN); json.add(target); json.add(pt); json.add(pm)`
    EndTurn { entity_id: i64, tp: i64, mp: i64 },

    // -----------------------------------------------------------------------
    // Movement
    // -----------------------------------------------------------------------
    /// `ActionMove` — `[10, entity_id, end_cell, [cell…]]`
    ///
    /// Java: `retour.add(Action.MOVE_TO); retour.add(leek); retour.add(end);
    ///        ArrayNode pathArray = …; retour.add(pathArray)`
    Move {
        entity_id: i64,
        end_cell: i32,
        path: Vec<i32>,
    },

    // -----------------------------------------------------------------------
    // Attack use
    // -----------------------------------------------------------------------
    /// `ActionUseWeapon` — `[16, cell, success]`
    ///
    /// Java: `retour.add(Action.USE_WEAPON); retour.add(cell); retour.add(success)`
    UseWeapon { cell: i32, success: i32 },

    /// `ActionUseChip` — `[12, chip_template, cell, success]`
    ///
    /// Java: `retour.add(Action.USE_CHIP); retour.add(chip); retour.add(cell); retour.add(success)`
    UseChip {
        chip_template: i32,
        cell: i32,
        success: i32,
    },

    /// `ActionSetWeapon` — `[13, weapon_template]`
    ///
    /// Java: `retour.add(Action.SET_WEAPON); retour.add(weapon)` (leek field stored but not emitted)
    SetWeapon { weapon_template: i32 },

    // -----------------------------------------------------------------------
    // Damage
    // -----------------------------------------------------------------------
    /// `ActionDamage` — `[damage_type_value, target_id, pv, erosion]`
    ///
    /// Java: `retour.add(type.value); retour.add(target); retour.add(pv); retour.add(erosion)`
    Damage {
        damage_type: DamageType,
        target_id: i64,
        pv: i32,
        erosion: i32,
    },

    // -----------------------------------------------------------------------
    // Heal / vitality
    // -----------------------------------------------------------------------
    /// `ActionHeal` — `[103, target_id, life]`
    ///
    /// Java: `retour.add(Action.HEAL); retour.add(target); retour.add(life)`
    Heal { target_id: i64, life: i32 },

    /// `ActionVitality` — `[104, target_id, life]`
    ///
    /// Java: `retour.add(Action.VITALITY); retour.add(target); retour.add(life)`
    Vitality { target_id: i64, life: i32 },

    /// `ActionNovaVitality` — `[112, target_id, life]`
    ///
    /// Java: `retour.add(Action.NOVA_VITALITY); retour.add(target); retour.add(life)`
    NovaVitality { target_id: i64, life: i32 },

    // -----------------------------------------------------------------------
    // Effects
    // -----------------------------------------------------------------------
    /// `ActionAddEffect` — `[type_id, item_id, log_id, caster_id, target_id, effect_id, value, turns]`
    /// or with optional modifiers field appended when `modifiers != 0`:
    /// `[…, modifiers]`
    ///
    /// The `type_id` is mapped: if the attack type was `TYPE_CHIP` the id
    /// becomes `ADD_CHIP_EFFECT` (302); if `TYPE_WEAPON` → `ADD_WEAPON_EFFECT`
    /// (301); otherwise the raw value is used.
    ///
    /// `log_id` is the auto-increment id returned by `ActionLog::get_effect_id()`
    /// and stored here so `StackEffect`/`UpdateEffect`/`RemoveEffect` can
    /// reference the same effect.
    ///
    /// Java: `retour.add(type); retour.add(itemID); retour.add(id);
    ///        retour.add(caster); retour.add(target); retour.add(effectID);
    ///        retour.add(value); retour.add(turns);
    ///        if (modifiers != 0) { retour.add(modifiers); }`
    AddEffect {
        /// Wire action-type id (already remapped from attack type)
        type_id: i64,
        item_id: i32,
        log_id: u32,
        caster_id: i64,
        target_id: i64,
        effect_id: i32,
        value: i32,
        turns: i32,
        modifiers: i32,
    },

    /// `ActionStackEffect` — `[14, log_id, value]`
    ///
    /// Java: `retour.add(Action.STACK_EFFECT); retour.add(id); retour.add(value)`
    StackEffect { log_id: u32, value: i32 },

    /// `ActionUpdateEffect` — `[304, log_id, value]`
    ///
    /// Java: `retour.add(Action.UPDATE_EFFECT); retour.add(id); retour.add(value)`
    UpdateEffect { log_id: u32, value: i32 },

    /// `ActionRemoveEffect` — `[303, log_id]`
    ///
    /// Java: `retour.add(Action.REMOVE_EFFECT); retour.add(id)`
    RemoveEffect { log_id: u32 },

    /// `ActionReduceEffects` — `[306, entity_id, value]`
    ///
    /// Java: `retour.add(Action.REDUCE_EFFECTS); retour.add(id); retour.add(value)`
    ReduceEffects { entity_id: i64, value: i32 },

    /// `ActionRemovePoisons` — `[307, entity_id]`
    ///
    /// Java: `retour.add(Action.REMOVE_POISONS); retour.add(id)`
    RemovePoisons { entity_id: i64 },

    /// `ActionRemoveShackles` — `[308, entity_id]`
    ///
    /// Java: `retour.add(Action.REMOVE_SHACKLES); retour.add(id)`
    RemoveShackles { entity_id: i64 },

    // -----------------------------------------------------------------------
    // Entity lifecycle
    // -----------------------------------------------------------------------
    /// `ActionEntityDie` — `[5, entity_id]` or `[5, entity_id, killer_id]`
    ///
    /// Java: `retour.add(Action.PLAYER_DEAD); retour.add(id);
    ///        if (killer != -1) { retour.add(killer); }`
    EntityDie {
        entity_id: i64,
        /// `-1` encodes "no killer"; the field is omitted from JSON when `-1`.
        killer_id: i64,
    },

    /// `ActionKill` — `[11, caster_id, target_id]`
    ///
    /// Java (note: Java stores `target.getFId()` in *both* `caster` and
    /// `target` — verbatim copy of the upstream bug):
    /// `retour.add(Action.KILL); retour.add(caster); retour.add(target)`
    Kill {
        /// In the Java source both fields are set to `target.getFId()`.
        caster_id: i64,
        target_id: i64,
    },

    /// `ActionResurrect` — `[105, owner_id, target_id, cell, life, max_life]`
    ///
    /// Java: `retour.add(Action.RESURRECT); retour.add(owner); retour.add(target);
    ///        retour.add(cell); retour.add(life); retour.add(max_life)`
    Resurrect {
        owner_id: i64,
        target_id: i64,
        cell: i32,
        life: i32,
        max_life: i32,
    },

    // -----------------------------------------------------------------------
    // Summon / invocation
    // -----------------------------------------------------------------------
    /// `ActionInvocation` — `[9, owner_id, summon_id, cell, result]`
    ///
    /// Java: `retour.add(Action.SUMMON); retour.add(owner); retour.add(target);
    ///        retour.add(cell); retour.add(result)`
    Invocation {
        owner_id: i64,
        summon_id: i64,
        cell: i32,
        result: i32,
    },

    // -----------------------------------------------------------------------
    // Chest
    // -----------------------------------------------------------------------
    /// `ActionChestOpened` — `[15, killer_id, chest_id, {resource_id: amount, …}]`
    ///
    /// Java: `retour.add(Action.CHEST_OPENED); retour.add(killer.getFId());
    ///        retour.add(chest.getFId()); retour.add(res)` where `res` is a
    ///        JSON object `{"resource_id": amount}`.
    ChestOpened {
        killer_id: i64,
        chest_id: i64,
        resources: HashMap<i32, i32>,
    },

    // -----------------------------------------------------------------------
    // Communication / display
    // -----------------------------------------------------------------------
    /// `ActionSay` — `[203, message]`
    ///
    /// Java: `retour.add(Action.SAY); retour.add(message.replaceAll("\t", "    "))`
    Say { message: String },

    /// `ActionShowCell` — `[205, cell, hex_color]`
    ///
    /// Java: `retour.add(Action.SHOW_CELL); retour.add(mCell);
    ///        retour.add(Util.getHexaColor(mColor))` where `getHexaColor`
    ///        formats the lower 24 bits as a zero-padded 6-char hex string.
    ShowCell {
        cell: i32,
        /// Raw 32-bit color; formatted as 6-char lowercase hex on the wire.
        color: u32,
    },

    /// `ActionLama` — `[201]`
    ///
    /// Java: `retour.add(Action.LAMA)`
    Lama,

    // -----------------------------------------------------------------------
    // Error / debug
    // -----------------------------------------------------------------------
    /// `ActionAIError` — `[1002, entity_id]`
    ///
    /// Java: `retour.add(Action.AI_ERROR); retour.add(id)` (id = -1 if leek == null)
    AiError { entity_id: i64 },
}

impl Action {
    /// Encode this action to the compact JSON array the Java generator emits.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            // [0]
            Self::StartFight { .. } => json!([START_FIGHT]),

            // [6, count]
            Self::NewTurn { count } => json!([NEW_TURN, count]),

            // [7, entity_id]
            Self::EntityTurn { entity_id } => json!([LEEK_TURN, entity_id]),

            // [8, entity_id, tp, mp]
            Self::EndTurn { entity_id, tp, mp } => json!([END_TURN, entity_id, tp, mp]),

            // [10, entity_id, end_cell, [path…]]
            Self::Move {
                entity_id,
                end_cell,
                path,
            } => {
                json!([MOVE_TO, entity_id, end_cell, path])
            }

            // [16, cell, success]
            Self::UseWeapon { cell, success } => json!([USE_WEAPON, cell, success]),

            // [12, chip_template, cell, success]
            Self::UseChip {
                chip_template,
                cell,
                success,
            } => {
                json!([USE_CHIP, chip_template, cell, success])
            }

            // [13, weapon_template]
            Self::SetWeapon { weapon_template } => json!([SET_WEAPON, weapon_template]),

            // [damage_type_value, target_id, pv, erosion]
            Self::Damage {
                damage_type,
                target_id,
                pv,
                erosion,
            } => {
                json!([damage_type.wire_value(), target_id, pv, erosion])
            }

            // [103, target_id, life]
            Self::Heal { target_id, life } => json!([HEAL, target_id, life]),

            // [104, target_id, life]
            Self::Vitality { target_id, life } => json!([VITALITY, target_id, life]),

            // [112, target_id, life]
            Self::NovaVitality { target_id, life } => json!([NOVA_VITALITY, target_id, life]),

            // [type_id, item_id, log_id, caster_id, target_id, effect_id, value, turns]
            // optionally [… , modifiers] when modifiers != 0
            Self::AddEffect {
                type_id,
                item_id,
                log_id,
                caster_id,
                target_id,
                effect_id,
                value,
                turns,
                modifiers,
            } => {
                let mut arr = vec![
                    json!(type_id),
                    json!(item_id),
                    json!(log_id),
                    json!(caster_id),
                    json!(target_id),
                    json!(effect_id),
                    json!(value),
                    json!(turns),
                ];
                if *modifiers != 0 {
                    arr.push(json!(modifiers));
                }
                Value::Array(arr)
            }

            // [14, log_id, value]
            Self::StackEffect { log_id, value } => json!([STACK_EFFECT, log_id, value]),

            // [304, log_id, value]
            Self::UpdateEffect { log_id, value } => json!([UPDATE_EFFECT, log_id, value]),

            // [303, log_id]
            Self::RemoveEffect { log_id } => json!([REMOVE_EFFECT, log_id]),

            // [306, entity_id, value]
            Self::ReduceEffects { entity_id, value } => {
                json!([REDUCE_EFFECTS, entity_id, value])
            }

            // [307, entity_id]
            Self::RemovePoisons { entity_id } => json!([REMOVE_POISONS, entity_id]),

            // [308, entity_id]
            Self::RemoveShackles { entity_id } => json!([REMOVE_SHACKLES, entity_id]),

            // [5, entity_id] or [5, entity_id, killer_id]
            Self::EntityDie {
                entity_id,
                killer_id,
            } => {
                if *killer_id == -1 {
                    json!([PLAYER_DEAD, entity_id])
                } else {
                    json!([PLAYER_DEAD, entity_id, killer_id])
                }
            }

            // [11, caster_id, target_id]
            Self::Kill {
                caster_id,
                target_id,
            } => json!([KILL, caster_id, target_id]),

            // [105, owner_id, target_id, cell, life, max_life]
            Self::Resurrect {
                owner_id,
                target_id,
                cell,
                life,
                max_life,
            } => {
                json!([RESURRECT, owner_id, target_id, cell, life, max_life])
            }

            // [9, owner_id, summon_id, cell, result]
            Self::Invocation {
                owner_id,
                summon_id,
                cell,
                result,
            } => {
                json!([SUMMON, owner_id, summon_id, cell, result])
            }

            // [15, killer_id, chest_id, {resource_id: amount, …}]
            Self::ChestOpened {
                killer_id,
                chest_id,
                resources,
            } => {
                // Build the resource object with string keys (mirrors Json.createObject() in Java)
                let res_obj: serde_json::Map<String, Value> = resources
                    .iter()
                    .map(|(k, v)| (k.to_string(), json!(v)))
                    .collect();
                json!([CHEST_OPENED, killer_id, chest_id, Value::Object(res_obj)])
            }

            // [203, message] — tabs replaced with 4 spaces, matching Java
            Self::Say { message } => {
                let normalized = message.replace('\t', "    ");
                json!([SAY, normalized])
            }

            // [205, cell, "rrggbb"]
            Self::ShowCell { cell, color } => {
                let hex = format!("{:06x}", color & 0x00FF_FFFF);
                json!([SHOW_CELL, cell, hex])
            }

            // [201]
            Self::Lama => json!([LAMA]),

            // [1002, entity_id]
            Self::AiError { entity_id } => json!([AI_ERROR, entity_id]),
        }
    }
}

// ---------------------------------------------------------------------------
// ActionLog (port of Actions.java)
// ---------------------------------------------------------------------------

/// The fight action log.  Mirrors `Actions.java`.
///
/// Collects every [`Action`] logged during a fight.  The `next_effect_id`
/// counter is the source-of-truth for `ActionAddEffect` log-ids; always use
/// [`ActionLog::create_effect`] to add effect-add events so the id is
/// consistent.
///
/// The `dead`, `ops`, and `times` side-tables from the Java are exposed as
/// plain `serde_json::Map` fields; the orchestrator populates them.
#[derive(Debug, Default)]
pub struct ActionLog {
    actions: Vec<Action>,
    next_effect_id: u32,

    /// Mirrors `Actions.dead` — entity-id → killer entity-id (string keys).
    pub dead: serde_json::Map<String, Value>,
    /// Mirrors `Actions.ops` — entity-id → op count (string keys).
    pub ops: serde_json::Map<String, Value>,
    /// Mirrors `Actions.times` (stored but not emitted by `toJSON`).
    pub times: serde_json::Map<String, Value>,
}

impl ActionLog {
    /// Create an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next effect id (mirrors `Actions.getEffectId()`).
    pub fn get_effect_id(&mut self) -> u32 {
        let id = self.next_effect_id;
        self.next_effect_id += 1;
        id
    }

    /// Index of the next action that will be pushed (mirrors `Actions.getNextId()`).
    #[must_use]
    pub fn get_next_id(&self) -> usize {
        self.actions.len()
    }

    /// Index of the most-recently pushed action (mirrors `Actions.currentID()`).
    ///
    /// # Panics
    /// Panics if no action has been logged yet (empty log).
    #[must_use]
    pub fn current_id(&self) -> usize {
        self.actions.len() - 1
    }

    /// Append an action (mirrors `Actions.log(Action)`).
    pub fn log(&mut self, action: Action) {
        self.actions.push(action);
    }

    /// Convenience: allocate an effect id, construct `AddEffect`, log it, and
    /// return the assigned id.  Mirrors `ActionAddEffect.createEffect(…)`.
    ///
    /// `attack_type` must be `ATTACK_TYPE_WEAPON` (1) or `ATTACK_TYPE_CHIP`
    /// (2) — or any other raw action-type id for special effects; the mapping
    /// to `ADD_WEAPON_EFFECT` / `ADD_CHIP_EFFECT` is done here exactly as the
    /// Java factory does.
    #[allow(clippy::too_many_arguments)]
    pub fn create_effect(
        &mut self,
        attack_type: i64,
        item_id: i32,
        caster_id: i64,
        target_id: i64,
        effect_id: i32,
        value: i32,
        turns: i32,
        modifiers: i32,
    ) -> u32 {
        let type_id = if attack_type == ATTACK_TYPE_CHIP {
            ADD_CHIP_EFFECT
        } else if attack_type == ATTACK_TYPE_WEAPON {
            ADD_WEAPON_EFFECT
        } else {
            attack_type
        };
        let log_id = self.get_effect_id();
        self.log(Action::AddEffect {
            type_id,
            item_id,
            log_id,
            caster_id,
            target_id,
            effect_id,
            value,
            turns,
            modifiers,
        });
        log_id
    }

    /// Record per-entity op counts (mirrors `Actions.addOpsAndTimes`).
    pub fn add_ops(&mut self, entity_id: i64, op_count: i64) {
        self.ops.insert(entity_id.to_string(), json!(op_count));
    }

    /// Serialize only the `actions` array (the outer `fight` object with
    /// `leeks`/`map`/`dead`/`ops` is assembled by the caller).
    ///
    /// Mirrors the inner loop of `Actions.toJSON()`:
    /// ```java
    /// ArrayNode json = Json.createArray();
    /// for (Action log : actions) { json.add(log.getJSON()); }
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Array(self.actions.iter().map(Action::to_json).collect())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // ActionStartFight
    // -----------------------------------------------------------------------

    #[test]
    fn start_fight_json() {
        // Java ActionStartFight.getJSON(): retour.add(Action.START_FIGHT) → [0]
        let a = Action::StartFight { team1: 1, team2: 2 };
        assert_eq!(a.to_json(), json!([0]));
    }

    // -----------------------------------------------------------------------
    // ActionNewTurn  — seed-42 real sample [6, 2]
    // -----------------------------------------------------------------------

    #[test]
    fn new_turn_json() {
        // Java ActionNewTurn.getJSON():
        //   retour.add(Action.NEW_TURN);  // 6
        //   retour.add(count);            // 2
        // → [6, 2]
        let a = Action::NewTurn { count: 2 };
        assert_eq!(a.to_json(), json!([6, 2]));
    }

    // -----------------------------------------------------------------------
    // ActionEntityTurn  — seed-42 real sample [7, 0]
    // -----------------------------------------------------------------------

    #[test]
    fn entity_turn_json() {
        // Java ActionEntityTurn.getJSON():
        //   retour.add(Action.LEEK_TURN);  // 7
        //   retour.add(id);                // 0
        // → [7, 0]
        let a = Action::EntityTurn { entity_id: 0 };
        assert_eq!(a.to_json(), json!([7, 0]));
    }

    #[test]
    fn entity_turn_null_entity_gives_minus_one() {
        // Java: if (leek == null) this.id = -1;
        let a = Action::EntityTurn { entity_id: -1 };
        assert_eq!(a.to_json(), json!([7, -1]));
    }

    // -----------------------------------------------------------------------
    // ActionEndTurn
    // -----------------------------------------------------------------------

    #[test]
    fn end_turn_json() {
        // Java ActionEndTurn.getJSON():
        //   json.add(Action.END_TURN);  // 8
        //   json.add(target);           // entity id
        //   json.add(pt);               // TP remaining
        //   json.add(pm);               // MP remaining
        // → [8, 0, 6, 7]  (matches seed-42 snippet [8,0,6,7])
        let a = Action::EndTurn {
            entity_id: 0,
            tp: 6,
            mp: 7,
        };
        assert_eq!(a.to_json(), json!([8, 0, 6, 7]));
    }

    // -----------------------------------------------------------------------
    // ActionMove  — seed-42 real sample [10,0,305,[302,285,303,321,304,287,305]]
    // -----------------------------------------------------------------------

    #[test]
    fn move_json() {
        // Java ActionMove.getJSON():
        //   retour.add(Action.MOVE_TO);  // 10
        //   retour.add(leek);            // entity id
        //   retour.add(end);             // last cell in path
        //   ArrayNode pathArray = …; for (int cell : path) pathArray.add(cell);
        //   retour.add(pathArray);
        // → [10, 0, 305, [302, 285, 303, 321, 304, 287, 305]]
        let path = vec![302, 285, 303, 321, 304, 287, 305];
        let a = Action::Move {
            entity_id: 0,
            end_cell: 305,
            path,
        };
        assert_eq!(
            a.to_json(),
            json!([10, 0, 305, [302, 285, 303, 321, 304, 287, 305]])
        );
    }

    // -----------------------------------------------------------------------
    // ActionUseWeapon  — seed-42 real sample [16, ?, ?] (type 16)
    // -----------------------------------------------------------------------

    #[test]
    fn use_weapon_json() {
        // Java ActionUseWeapon.getJSON():
        //   retour.add(Action.USE_WEAPON);  // 16
        //   retour.add(cell);
        //   retour.add(success);
        let a = Action::UseWeapon {
            cell: 42,
            success: 1,
        };
        assert_eq!(a.to_json(), json!([16, 42, 1]));
    }

    // -----------------------------------------------------------------------
    // ActionUseChip
    // -----------------------------------------------------------------------

    #[test]
    fn use_chip_json() {
        // Java ActionUseChip.getJSON():
        //   retour.add(Action.USE_CHIP);  // 12
        //   retour.add(chip);             // chip template id
        //   retour.add(cell);
        //   retour.add(success);
        let a = Action::UseChip {
            chip_template: 7,
            cell: 100,
            success: 1,
        };
        assert_eq!(a.to_json(), json!([12, 7, 100, 1]));
    }

    // -----------------------------------------------------------------------
    // ActionSetWeapon  — seed-42 real sample [13, 37]
    // -----------------------------------------------------------------------

    #[test]
    fn set_weapon_json() {
        // Java ActionSetWeapon.getJSON():
        //   retour.add(Action.SET_WEAPON);  // 13
        //   retour.add(weapon);             // template id 37
        // → [13, 37]
        let a = Action::SetWeapon {
            weapon_template: 37,
        };
        assert_eq!(a.to_json(), json!([13, 37]));
    }

    // -----------------------------------------------------------------------
    // ActionDamage — DamageType variants
    // -----------------------------------------------------------------------

    #[test]
    fn damage_direct_json() {
        // Java ActionDamage.getJSON():
        //   retour.add(type.value);  // DamageType.DIRECT.value = 101
        //   retour.add(target);
        //   retour.add(pv);
        //   retour.add(erosion);
        let a = Action::Damage {
            damage_type: DamageType::Direct,
            target_id: 5,
            pv: 42,
            erosion: 3,
        };
        assert_eq!(a.to_json(), json!([101, 5, 42, 3]));
    }

    #[test]
    fn damage_nova_json() {
        let a = Action::Damage {
            damage_type: DamageType::Nova,
            target_id: 1,
            pv: 10,
            erosion: 0,
        };
        assert_eq!(a.to_json(), json!([107, 1, 10, 0]));
    }

    #[test]
    fn damage_poison_json() {
        // DamageType.POISON.value = 110 (same as AFTEREFFECT in Java)
        let a = Action::Damage {
            damage_type: DamageType::Poison,
            target_id: 2,
            pv: 5,
            erosion: 0,
        };
        assert_eq!(a.to_json(), json!([110, 2, 5, 0]));
    }

    // -----------------------------------------------------------------------
    // ActionAddEffect + ActionStackEffect logID linkage
    // -----------------------------------------------------------------------

    #[test]
    fn add_effect_weapon_no_modifiers() {
        // Java ActionAddEffect: type == Attack.TYPE_WEAPON → ADD_WEAPON_EFFECT (301)
        // modifiers == 0 → NOT appended
        // Wire: [301, item_id, log_id, caster_id, target_id, effect_id, value, turns]
        let a = Action::AddEffect {
            type_id: ADD_WEAPON_EFFECT,
            item_id: 5,
            log_id: 0,
            caster_id: 1,
            target_id: 2,
            effect_id: 10,
            value: 20,
            turns: 3,
            modifiers: 0,
        };
        assert_eq!(a.to_json(), json!([301, 5, 0, 1, 2, 10, 20, 3]));
    }

    #[test]
    fn add_effect_chip_with_modifiers() {
        // Java ActionAddEffect: type == Attack.TYPE_CHIP → ADD_CHIP_EFFECT (302)
        // modifiers != 0 → appended as 9th element
        // Wire: [302, item_id, log_id, caster_id, target_id, effect_id, value, turns, modifiers]
        let a = Action::AddEffect {
            type_id: ADD_CHIP_EFFECT,
            item_id: 7,
            log_id: 1,
            caster_id: 3,
            target_id: 4,
            effect_id: 11,
            value: 15,
            turns: 2,
            modifiers: 8,
        };
        assert_eq!(a.to_json(), json!([302, 7, 1, 3, 4, 11, 15, 2, 8]));
    }

    #[test]
    fn add_effect_logid_linkage_with_stack_and_remove() {
        // Demonstrate the logID mechanic:
        //   1. create_effect allocates id 0, logs AddEffect
        //   2. StackEffect references id 0
        //   3. RemoveEffect references id 0
        //
        // Java: int r = logs.getEffectId(); ActionAddEffect effect = new ActionAddEffect(…, r, …);
        //       logs.log(effect); return r;  → callers use r for StackEffect/RemoveEffect/UpdateEffect
        let mut log = ActionLog::new();
        let id = log.create_effect(ATTACK_TYPE_WEAPON, 5, 1, 2, 10, 20, 3, 0);
        assert_eq!(id, 0, "first effect id must be 0");

        log.log(Action::StackEffect {
            log_id: id,
            value: 5,
        });
        log.log(Action::RemoveEffect { log_id: id });

        let actions = log.to_json();
        // AddEffect → [301, 5, 0, 1, 2, 10, 20, 3]
        assert_eq!(actions[0], json!([301, 5, 0, 1, 2, 10, 20, 3]));
        // StackEffect → [14, 0, 5]
        assert_eq!(actions[1], json!([14, 0, 5]));
        // RemoveEffect → [303, 0]
        assert_eq!(actions[2], json!([303, 0]));
    }

    #[test]
    fn second_effect_id_increments() {
        let mut log = ActionLog::new();
        let id0 = log.create_effect(ATTACK_TYPE_CHIP, 7, 1, 2, 11, 10, 2, 0);
        let id1 = log.create_effect(ATTACK_TYPE_CHIP, 8, 1, 3, 12, 8, 1, 0);
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
        // UpdateEffect references second effect
        log.log(Action::UpdateEffect {
            log_id: id1,
            value: 6,
        });
        let actions = log.to_json();
        assert_eq!(actions[2], json!([304, 1, 6]));
    }

    // -----------------------------------------------------------------------
    // ActionEntityDie — with and without killer
    // -----------------------------------------------------------------------

    #[test]
    fn entity_die_with_killer() {
        // Java ActionEntityDie.getJSON():
        //   retour.add(Action.PLAYER_DEAD);  // 5
        //   retour.add(id);
        //   if (killer != -1) { retour.add(killer); }
        let a = Action::EntityDie {
            entity_id: 3,
            killer_id: 1,
        };
        assert_eq!(a.to_json(), json!([5, 3, 1]));
    }

    #[test]
    fn entity_die_no_killer() {
        let a = Action::EntityDie {
            entity_id: 3,
            killer_id: -1,
        };
        assert_eq!(a.to_json(), json!([5, 3]));
    }

    // -----------------------------------------------------------------------
    // ActionSay
    // -----------------------------------------------------------------------

    #[test]
    fn say_json_no_tabs() {
        // Java ActionSay.getJSON():
        //   retour.add(Action.SAY);  // 203
        //   retour.add(message.replaceAll("\t", "    "));
        let a = Action::Say {
            message: "hello".to_owned(),
        };
        assert_eq!(a.to_json(), json!([203, "hello"]));
    }

    #[test]
    fn say_json_tabs_replaced() {
        let a = Action::Say {
            message: "a\tb".to_owned(),
        };
        assert_eq!(a.to_json(), json!([203, "a    b"]));
    }

    // -----------------------------------------------------------------------
    // ActionSetWeapon (already covered above) + additional ActionUseChip order
    // -----------------------------------------------------------------------

    #[test]
    fn use_chip_field_order() {
        // Java: USE_CHIP(12), chip, cell, success — NOT cell first
        let a = Action::UseChip {
            chip_template: 3,
            cell: 50,
            success: 0,
        };
        let v = a.to_json();
        assert_eq!(v[0], json!(12)); // USE_CHIP
        assert_eq!(v[1], json!(3)); // chip_template
        assert_eq!(v[2], json!(50)); // cell
        assert_eq!(v[3], json!(0)); // success
    }

    // -----------------------------------------------------------------------
    // ActionLog: to_json assembles all actions in order
    // -----------------------------------------------------------------------

    #[test]
    fn action_log_to_json_order() {
        let mut log = ActionLog::new();
        log.log(Action::StartFight { team1: 1, team2: 2 });
        log.log(Action::NewTurn { count: 1 });
        log.log(Action::EntityTurn { entity_id: 0 });

        let v = log.to_json();
        assert!(v.is_array());
        assert_eq!(v[0], json!([0]));
        assert_eq!(v[1], json!([6, 1]));
        assert_eq!(v[2], json!([7, 0]));
    }

    // -----------------------------------------------------------------------
    // ShowCell — hex color formatting
    // -----------------------------------------------------------------------

    #[test]
    fn show_cell_hex_color() {
        // Java Util.getHexaColor(color): Long.toString(color & 0xFFFFFF, 16), zero-padded to 6
        let a = Action::ShowCell {
            cell: 10,
            color: 0x00_FF00,
        };
        assert_eq!(a.to_json(), json!([205, 10, "00ff00"]));
    }

    #[test]
    fn show_cell_hex_color_zero_padded() {
        let a = Action::ShowCell {
            cell: 5,
            color: 0x0000_00FF,
        };
        assert_eq!(a.to_json(), json!([205, 5, "0000ff"]));
    }

    #[test]
    fn show_cell_hex_strips_alpha() {
        // Only lower 24 bits used
        let a = Action::ShowCell {
            cell: 0,
            color: 0xFF_FF0000,
        };
        assert_eq!(a.to_json(), json!([205, 0, "ff0000"]));
    }

    // -----------------------------------------------------------------------
    // Misc small variants
    // -----------------------------------------------------------------------

    #[test]
    fn lama_json() {
        assert_eq!(Action::Lama.to_json(), json!([201]));
    }

    #[test]
    fn ai_error_json() {
        assert_eq!(Action::AiError { entity_id: 5 }.to_json(), json!([1002, 5]));
    }

    #[test]
    fn resurrect_json() {
        let a = Action::Resurrect {
            owner_id: 1,
            target_id: 2,
            cell: 10,
            life: 50,
            max_life: 100,
        };
        assert_eq!(a.to_json(), json!([105, 1, 2, 10, 50, 100]));
    }

    #[test]
    fn invocation_json() {
        let a = Action::Invocation {
            owner_id: 0,
            summon_id: 5,
            cell: 20,
            result: 1,
        };
        assert_eq!(a.to_json(), json!([9, 0, 5, 20, 1]));
    }

    #[test]
    fn chest_opened_json() {
        let mut resources = HashMap::new();
        resources.insert(1, 5);
        let a = Action::ChestOpened {
            killer_id: 0,
            chest_id: 3,
            resources,
        };
        let v = a.to_json();
        assert_eq!(v[0], json!(15));
        assert_eq!(v[1], json!(0));
        assert_eq!(v[2], json!(3));
        assert_eq!(v[3]["1"], json!(5));
    }

    #[test]
    fn remove_poisons_json() {
        assert_eq!(
            Action::RemovePoisons { entity_id: 2 }.to_json(),
            json!([307, 2])
        );
    }

    #[test]
    fn remove_shackles_json() {
        assert_eq!(
            Action::RemoveShackles { entity_id: 3 }.to_json(),
            json!([308, 3])
        );
    }

    #[test]
    fn reduce_effects_json() {
        assert_eq!(
            Action::ReduceEffects {
                entity_id: 1,
                value: 50
            }
            .to_json(),
            json!([306, 1, 50])
        );
    }

    #[test]
    fn heal_json() {
        assert_eq!(
            Action::Heal {
                target_id: 4,
                life: 30
            }
            .to_json(),
            json!([103, 4, 30])
        );
    }

    #[test]
    fn vitality_json() {
        assert_eq!(
            Action::Vitality {
                target_id: 4,
                life: 50
            }
            .to_json(),
            json!([104, 4, 50])
        );
    }

    #[test]
    fn nova_vitality_json() {
        assert_eq!(
            Action::NovaVitality {
                target_id: 2,
                life: 10
            }
            .to_json(),
            json!([112, 2, 10])
        );
    }

    #[test]
    fn kill_json() {
        // Java ActionKill: both caster and target fields are set to target.getFId()
        let a = Action::Kill {
            caster_id: 0,
            target_id: 0,
        };
        assert_eq!(a.to_json(), json!([11, 0, 0]));
    }

    #[test]
    fn update_effect_json() {
        assert_eq!(
            Action::UpdateEffect {
                log_id: 3,
                value: 7
            }
            .to_json(),
            json!([304, 3, 7])
        );
    }

    // -----------------------------------------------------------------------
    // ActionLog ops bookkeeping
    // -----------------------------------------------------------------------

    #[test]
    fn action_log_add_ops() {
        let mut log = ActionLog::new();
        log.add_ops(0, 1234);
        log.add_ops(1, 5678);
        assert_eq!(log.ops["0"], json!(1234));
        assert_eq!(log.ops["1"], json!(5678));
    }
}
