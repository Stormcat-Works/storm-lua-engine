// cargo xtask generate によって自動生成されています。
export const VEHICLE_API_CATALOG = [
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorOcean",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorShallows",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorLand",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorGrass",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorSand",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorSnow",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorRock",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "map-palette",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setMapColorGravel",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "queue-http",
    "parameters": "port: integer, request: string",
    "path": "async.httpGet",
    "phase": "any",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "read-signal",
    "parameters": "channel: integer",
    "path": "input.getNumber",
    "phase": "tick",
    "returns": "number"
  },
  {
    "availability": "implemented",
    "effect": "read-signal",
    "parameters": "channel: integer",
    "path": "input.getBool",
    "phase": "tick",
    "returns": "boolean"
  },
  {
    "availability": "implemented",
    "effect": "write-signal",
    "parameters": "channel: integer, value: number",
    "path": "output.setNumber",
    "phase": "tick",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "write-signal",
    "parameters": "channel: integer, value: boolean",
    "path": "output.setBool",
    "phase": "tick",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "read-property",
    "parameters": "label: string",
    "path": "property.getNumber",
    "phase": "any",
    "returns": "number"
  },
  {
    "availability": "implemented",
    "effect": "read-property",
    "parameters": "label: string",
    "path": "property.getBool",
    "phase": "any",
    "returns": "boolean"
  },
  {
    "availability": "implemented",
    "effect": "read-property",
    "parameters": "label: string",
    "path": "property.getText",
    "phase": "any",
    "returns": "string"
  },
  {
    "availability": "implemented",
    "effect": "read-context",
    "parameters": "",
    "path": "screen.getWidth",
    "phase": "draw",
    "returns": "number"
  },
  {
    "availability": "implemented",
    "effect": "read-context",
    "parameters": "",
    "path": "screen.getHeight",
    "phase": "draw",
    "returns": "number"
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "r: number, g: number, b: number, a?: number",
    "path": "screen.setColor",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "",
    "path": "screen.drawClear",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x1: number, y1: number, x2: number, y2: number",
    "path": "screen.drawLine",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x: number, y: number, width: number, height: number",
    "path": "screen.drawRect",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x: number, y: number, width: number, height: number",
    "path": "screen.drawRectF",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x: number, y: number, radius: number",
    "path": "screen.drawCircle",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x: number, y: number, radius: number",
    "path": "screen.drawCircleF",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x1: number, y1: number, x2: number, y2: number, x3: number, y3: number",
    "path": "screen.drawTriangle",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x1: number, y1: number, x2: number, y2: number, x3: number, y3: number",
    "path": "screen.drawTriangleF",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x: number, y: number, text: string",
    "path": "screen.drawText",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "draw-command",
    "parameters": "x: number, y: number, width: number, height: number, text: string, horizontalAlign?: number, verticalAlign?: number",
    "path": "screen.drawTextBox",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "requires-provider",
    "effect": "host-service",
    "parameters": "x: number, y: number, zoom: number",
    "path": "screen.drawMap",
    "phase": "draw",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "coordinate-conversion",
    "parameters": "mapX: number, mapY: number, zoom: number, screenWidth: number, screenHeight: number, pixelX: number, pixelY: number",
    "path": "map.screenToMap",
    "phase": "any",
    "returns": "number, number"
  },
  {
    "availability": "implemented",
    "effect": "coordinate-conversion",
    "parameters": "mapX: number, mapY: number, zoom: number, screenWidth: number, screenHeight: number, worldX: number, worldY: number",
    "path": "map.mapToScreen",
    "phase": "any",
    "returns": "number, number"
  },
  {
    "availability": "host-extension",
    "effect": "log",
    "parameters": "...: any",
    "path": "print",
    "phase": "any",
    "returns": ""
  },
  {
    "availability": "implemented",
    "effect": "log",
    "parameters": "...: any",
    "path": "debug.log",
    "phase": "any",
    "returns": ""
  }
] as const;
export const ADDON_API_CATALOG = {
  "events": [
    "onCustomCommand",
    "onChatMessage",
    "onPlayerJoin",
    "onPlayerSit",
    "onPlayerUnsit",
    "onCharacterSit",
    "onCharacterUnsit",
    "onCharacterPickup",
    "onCreatureSit",
    "onCreatureUnsit",
    "onCreaturePickup",
    "onEquipmentPickup",
    "onEquipmentDrop",
    "onPlayerRespawn",
    "onPlayerLeave",
    "onToggleMap",
    "onPlayerDie",
    "onGroupSpawn",
    "onVehicleSpawn",
    "onVehicleDespawn",
    "onVehicleLoad",
    "onVehicleUnload",
    "onVehicleTeleport",
    "onObjectLoad",
    "onObjectUnload",
    "onButtonPress",
    "onSpawnAddonComponent",
    "onVehicleDamaged",
    "onFireExtinguished",
    "onForestFireSpawned",
    "onForestFireExtinguished",
    "onTornado",
    "onMeteor",
    "onTsunami",
    "onWhirlpool",
    "onVolcano",
    "onOilSpill",
    "onClearOilSpill"
  ],
  "functions": [
    "property.checkbox",
    "property.slider",
    "server.httpGet",
    "debug.log",
    "matrix.identity",
    "matrix.translation",
    "matrix.position",
    "matrix.multiply",
    "matrix.transpose",
    "matrix.invert",
    "matrix.distance",
    "matrix.multiplyXYZW",
    "matrix.rotationX",
    "matrix.rotationY",
    "matrix.rotationZ",
    "matrix.rotationToFaceXZ"
  ],
  "profile": "addon"
} as const;
export const ADDON_EVENTS = [
  "onCustomCommand",
  "onChatMessage",
  "onPlayerJoin",
  "onPlayerSit",
  "onPlayerUnsit",
  "onCharacterSit",
  "onCharacterUnsit",
  "onCharacterPickup",
  "onCreatureSit",
  "onCreatureUnsit",
  "onCreaturePickup",
  "onEquipmentPickup",
  "onEquipmentDrop",
  "onPlayerRespawn",
  "onPlayerLeave",
  "onToggleMap",
  "onPlayerDie",
  "onGroupSpawn",
  "onVehicleSpawn",
  "onVehicleDespawn",
  "onVehicleLoad",
  "onVehicleUnload",
  "onVehicleTeleport",
  "onObjectLoad",
  "onObjectUnload",
  "onButtonPress",
  "onSpawnAddonComponent",
  "onVehicleDamaged",
  "onFireExtinguished",
  "onForestFireSpawned",
  "onForestFireExtinguished",
  "onTornado",
  "onMeteor",
  "onTsunami",
  "onWhirlpool",
  "onVolcano",
  "onOilSpill",
  "onClearOilSpill"
] as const;
export const COMPONENT_API_CATALOG = {
  "callbacks": [
    "onParse",
    "onAddToSimulation",
    "onTick",
    "onRender",
    "onRemoveFromSimulation"
  ],
  "functions": [
    {
      "availability": "implemented",
      "effect": "log",
      "parameters": "message: string | number",
      "path": "debug.log",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "parse-definition",
      "parameters": "key: string, value: boolean",
      "path": "parser.parseBool",
      "phase": "any",
      "returns": "boolean, boolean"
    },
    {
      "availability": "requires-provider",
      "effect": "parse-definition",
      "parameters": "key: string, value: number",
      "path": "parser.parseNumber",
      "phase": "any",
      "returns": "number, boolean"
    },
    {
      "availability": "requires-provider",
      "effect": "parse-definition",
      "parameters": "key: string, value: string",
      "path": "parser.parseString",
      "phase": "any",
      "returns": "string, boolean"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "a: table, b: table",
      "path": "matrix.multiply",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "matrix: table, x: number, y: number, z: number, w: number",
      "path": "matrix.multiplyXYZW",
      "phase": "any",
      "returns": "number, number, number, number"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "matrix: table",
      "path": "matrix.invert",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "matrix: table",
      "path": "matrix.transpose",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "",
      "path": "matrix.identity",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "x: number, y: number, z: number",
      "path": "matrix.scale",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "angle: number",
      "path": "matrix.rotationX",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "angle: number",
      "path": "matrix.rotationY",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "angle: number",
      "path": "matrix.rotationZ",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "x: number, y: number, z: number",
      "path": "matrix.translation",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "matrix: table",
      "path": "matrix.position",
      "phase": "any",
      "returns": "number, number, number"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "a: table, b: table",
      "path": "matrix.distance",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "implemented",
      "effect": "matrix",
      "parameters": "x: number, z: number",
      "path": "matrix.rotationToFaceXZ",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "requires-provider",
      "effect": "logic-slot",
      "parameters": "slot: integer, value: boolean",
      "path": "component.setOutputLogicSlotBool",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "logic-slot",
      "parameters": "slot: integer, value: number",
      "path": "component.setOutputLogicSlotFloat",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "logic-slot",
      "parameters": "slot: integer, value: table",
      "path": "component.setOutputLogicSlotComposite",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "logic-slot",
      "parameters": "slot: integer",
      "path": "component.getInputLogicSlotBool",
      "phase": "any",
      "returns": "boolean"
    },
    {
      "availability": "requires-provider",
      "effect": "logic-slot",
      "parameters": "slot: integer",
      "path": "component.getInputLogicSlotFloat",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "logic-slot",
      "parameters": "slot: integer",
      "path": "component.getInputLogicSlotComposite",
      "phase": "any",
      "returns": "table"
    },
    {
      "availability": "requires-provider",
      "effect": "torque-slot",
      "parameters": "slot: integer",
      "path": "component.slotTorqueIsConnected",
      "phase": "any",
      "returns": "boolean"
    },
    {
      "availability": "requires-provider",
      "effect": "torque-slot",
      "parameters": "slot: integer, value: number, dt: number",
      "path": "component.slotTorqueApplyMomentum",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "torque-slot",
      "parameters": "integer, integer",
      "path": "component.slotTorqueCreateBridge",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "torque-slot",
      "parameters": "slot: integer",
      "path": "component.slotTorqueDestroyBridge",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "torque-slot",
      "parameters": "integer, number",
      "path": "component.slotTorqueSetBridgeFactor",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "torque-slot",
      "parameters": "integer, number",
      "path": "component.slotTorqueSetBridgeRatio",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-slot",
      "parameters": "integer, integer, number, number, integer, integer, integer, integer",
      "path": "component.slotFluidResolveFlow",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-slot",
      "parameters": "integer, integer, number, number, integer, integer, integer",
      "path": "component.slotFluidResolveFlowToSlot",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "electric-slot",
      "parameters": "slot: integer",
      "path": "component.slotElectricIsConnected",
      "phase": "any",
      "returns": "boolean"
    },
    {
      "availability": "requires-provider",
      "effect": "electric-slot",
      "parameters": "slot: integer, charge: number",
      "path": "component.slotElectricAddCharge",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "electric-slot",
      "parameters": "slot: integer, charge: number",
      "path": "component.slotElectricRemoveCharge",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "electric-slot",
      "parameters": "slot: integer",
      "path": "component.slotElectricGetChargeFactor",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "sound",
      "parameters": "integer, integer, number, number, number, number, number, number, number, number",
      "path": "component.sfxPlayOnce",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "sound",
      "parameters": "integer, integer, number, number, number, number, number, number, number, number",
      "path": "component.sfxPlayLoop",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "sound",
      "parameters": "integer, number, number, number, number, number, number, number",
      "path": "component.sfxUpdate",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "sound",
      "parameters": "integer",
      "path": "component.sfxStop",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "from: integer, to: integer, volume: number",
      "path": "component.fluidContentsTransferVolume",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "from: integer, to: integer, volume: number, exceptType: integer",
      "path": "component.fluidContentsTransferVolumeExceptType",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "slot: integer, fluidType: integer, volume: number",
      "path": "component.fluidContentsSetFluidTypeVolume",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "slot: integer, fluidType: integer",
      "path": "component.fluidContentsGetFluidTypeVolume",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "slot: integer",
      "path": "component.fluidContentsGetVolume",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "slot: integer, capacity: number",
      "path": "component.fluidContentsSetCapacity",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "slot: integer",
      "path": "component.fluidContentsGetCapacity",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "fluid-contents",
      "parameters": "slot: integer",
      "path": "component.fluidContentsGetPressure",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "heat",
      "parameters": "x: number, y: number, z: number, radius: number, temperature: number",
      "path": "component.heaterSetSphere",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "heat",
      "parameters": "matrix: table, number, number, number, number",
      "path": "component.heaterSetOrientedBox",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "heat",
      "parameters": "temperature: number",
      "path": "component.heaterSetTemperature",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "heat",
      "parameters": "integer, number",
      "path": "component.heaterSetFactor",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "render",
      "parameters": "matrix: table",
      "path": "component.renderMesh0",
      "phase": "draw",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "render",
      "parameters": "matrix: table",
      "path": "component.renderMesh1",
      "phase": "draw",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "render",
      "parameters": "matrix: table",
      "path": "component.renderMesh2",
      "phase": "draw",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "render",
      "parameters": "number, number, number, number, number, number, number, boolean, boolean",
      "path": "component.renderLight",
      "phase": "draw",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "render",
      "parameters": "matrix: table, number, number",
      "path": "component.renderLaser",
      "phase": "draw",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "physics",
      "parameters": "x: number, y: number, z: number, fx: number, fy: number, fz: number, local: boolean",
      "path": "component.physicsImpulse",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "physics",
      "parameters": "x: number, y: number, z: number, dx: number, dy: number, dz: number",
      "path": "component.physicsRaycast",
      "phase": "any",
      "returns": "any"
    },
    {
      "availability": "requires-provider",
      "effect": "physics",
      "parameters": "",
      "path": "component.physicsGetLinearVelocity",
      "phase": "any",
      "returns": "number, number, number"
    },
    {
      "availability": "requires-provider",
      "effect": "physics",
      "parameters": "x: number, y: number, z: number",
      "path": "component.physicsGetLinearVelocityAtPoint",
      "phase": "any",
      "returns": "number, number, number"
    },
    {
      "availability": "requires-provider",
      "effect": "physics",
      "parameters": "",
      "path": "component.physicsGetAngularVelocity",
      "phase": "any",
      "returns": "number, number, number"
    },
    {
      "availability": "requires-provider",
      "effect": "physics",
      "parameters": "integer, integer, number, number, number, number, number, number",
      "path": "component.spawnProjectile",
      "phase": "any",
      "returns": ""
    },
    {
      "availability": "requires-provider",
      "effect": "environment-query",
      "parameters": "number, number, number, number",
      "path": "component.getSubmergenceFactor",
      "phase": "any",
      "returns": "number"
    },
    {
      "availability": "requires-provider",
      "effect": "environment-query",
      "parameters": "",
      "path": "component.getWindVelocity",
      "phase": "any",
      "returns": "number, number, number"
    }
  ],
  "profile": "component"
} as const;
