import nodemailer from "nodemailer";

export type SendInviteEmail = (opts: {
  to: string;
  inviteToken: string;
  folderName: string;
}) => Promise<void>;

export type CreateSendInviteEmailOptions = {
  smtpHost?: string;
  smtpPort?: number;
  smtpUser?: string;
  smtpPass: string;
  from: string;
  appBaseUrl: string;
};

export function createSendInviteEmail(opts: CreateSendInviteEmailOptions): SendInviteEmail {
  const transporter = nodemailer.createTransport({
    host: opts.smtpHost ?? "smtp.mx.cloudflare.net",
    port: opts.smtpPort ?? 465,
    secure: opts.smtpPort === undefined || opts.smtpPort === 465,
    auth: {
      user: opts.smtpUser ?? "apitoken",
      pass: opts.smtpPass,
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
