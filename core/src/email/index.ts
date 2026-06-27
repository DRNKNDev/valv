import nodemailer from "nodemailer";

export type SendInviteEmail = (opts: {
  to: string;
  inviteToken: string;
  folderName: string;
}) => Promise<void>;

export type CreateSendInviteEmailOptions = {
  apiToken: string;
  from: string;
  appBaseUrl: string;
};

export function createSendInviteEmail(opts: CreateSendInviteEmailOptions): SendInviteEmail {
  const transporter = nodemailer.createTransport({
    host: "smtp.mx.cloudflare.net",
    port: 465,
    secure: true,
    auth: {
      user: "apitoken",
      pass: opts.apiToken,
    },
  });

  return async ({ to, inviteToken, folderName }) => {
    const acceptUrl = `${opts.appBaseUrl.replace(/\/$/, "")}/invite/${inviteToken}`;
    await transporter.sendMail({
      from: opts.from,
      to,
      subject: `You've been invited to "${folderName}"`,
      text: `Accept your invite: ${acceptUrl}`,
    });
  };
}
