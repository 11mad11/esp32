import { program } from "commander";
import { createReadStream, globSync, readFileSync, statSync } from "fs";
import { select } from "@inquirer/prompts";
import mqtt, { MqttClient } from "mqtt";
import { decode } from "jsonwebtoken";
import { execSync } from "child_process";
import cr32 from "crc-32"
import { config } from "dotenv";

config();


program.action(async () => {

    const devices = globSync("../devices/*").map(f => {
        const content = readFileSync(f);
        const env = content.toString().split("\n").reduce((acc, line) => {
            const [key, value] = line.split("=");
            if (key && value) {
                acc[key.trim()] = value.trim();
            }
            return acc;
        }, {} as Record<string, string>);
        return { env, name: env.desc ?? env.id ?? f };
    })

    const selected = await select({
        message: "Select a device",
        choices: devices.map((d, i) => ({ value: i, name: d.name }))
    });

    const device = devices[selected];
    console.log(device);

    const id = decode(process.env.TOKEN).id;
    if (typeof id !== "string")
        throw new Error("Bad Token");

    execSync("./compile.sh");
    const mq = mqtt.connect("mqtt://ssca.desrochers.space", {
        username: process.env.TOKEN,
        password: " ",
        clientId: id
    });

    await new Promise(r => mq.on("connect", r));

    const up = new Uploader(mq, device.env.ID);
    mq.on("message", (topic, payload, packet) => {
        clearInterval(start_inte);
        switch (topic) {
            case "iot/" + device.env.ID + "/ota/ready":
                up.ready();
                break;
            default:
                console.log(topic, payload.toString());
        }
    })

    mq.subscribe("iot/" + device.env.ID + "/ota/log");
    mq.subscribe("iot/" + device.env.ID + "/ota/ready");

    const start_inte = setInterval(() => {
        const pk = JSON.stringify({
            size: statSync("./firmware.bin").size,
            target_crc: cr32.buf(readFileSync("./firmware.bin")) >>> 0, // Ensure unsigned 32-bit integer
        });
        console.log("Sending start", pk);
        mq.publish("iot/" + device.env.ID + "/ota/start", pk);
    }, 2000)
})

program.parse();

class Uploader {
    future = 1;
    chunk_size = 4096;

    rs = createReadStream("./firmware.bin", { highWaterMark: 4096 });
    readyCnt = 0;
    sendCnt = 0;
    totalBytes = 0;

    async readChunk(): Promise<Buffer> {
        return new Promise((resolve, reject) => {
            const chunk = this.rs.read(this.chunk_size);
            if (chunk) {
                resolve(chunk);
            } else {
                this.rs.once("readable", () => {
                    const nextChunk = this.rs.read(this.chunk_size);
                    if (nextChunk) {
                        resolve(nextChunk);
                    } else {
                        reject(new Error("No more data to read"));
                    }
                });
                this.rs.once("end", () => reject(new Error("Stream ended")));
                this.rs.once("error", (err) => reject(err));
            }
        });
    }

    constructor(
        private mq: MqttClient,
        private device: string
    ) {
        const fileSize = statSync("./firmware.bin").size;

        let last = 0;
        let acc = 0;
        setInterval(() => {
            const percentage = ((this.totalBytes / fileSize) * 100).toFixed(2);
            acc = (this.totalBytes - last + acc) / 2;
            console.log(`Data rate: ${acc} bytes/sec, Progress: ${percentage}%`);
            last = this.totalBytes
        }, 1000);
    }

    async ready() {
        this.readyCnt++;
        while (this.sendCnt - this.future < this.readyCnt) {
            this.sendCnt++;
            await this.send();
        }
    }

    async send() {
        const buf = await this.readChunk();
        this.totalBytes += buf.length;
        this.mq.publish("iot/" + this.device + "/ota/data", buf);
    }
}