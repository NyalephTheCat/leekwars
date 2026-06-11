import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4962 extends AI {
public AI_4962() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
session.rebindAll(this);
var u_a = session.getVariable("a");
var u_m = session.getVariable("m");
ArrayLeekValue u_t = (ArrayLeekValue) ops(toArray(0, new ArrayLeekValue(AI_4962.this)), 1);
session.setVariable(AI_4962.this, "t", u_t);
return null;
}
protected String getAIString() { return "<snippet 4962>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4962>", };}

protected int[] getErrorFilesID() { return new int[] {4962, };}

}
