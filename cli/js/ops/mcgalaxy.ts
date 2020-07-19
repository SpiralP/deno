import { sendSync } from "./dispatch_json.ts";

interface MCGalaxyTest {
  test: string;
}

export function test(): MCGalaxyTest {
  return sendSync("op_mcgalaxy_test");
}
